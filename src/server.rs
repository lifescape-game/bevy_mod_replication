pub(super) mod despawn_tracker;
pub(super) mod removal_tracker;

use std::time::Duration;

use bevy::{
    ecs::{
        archetype::{Archetype, ArchetypeId},
        component::{ComponentId, StorageType, Tick},
        storage::{SparseSets, Table},
        system::{Local, SystemChangeTick},
    },
    prelude::*,
    time::common_conditions::on_timer,
    utils::HashMap,
};
use bevy_renet::{
    renet::{RenetClient, RenetServer, ServerEvent},
    transport::NetcodeServerPlugin,
    RenetServerPlugin,
};
use derive_more::Constructor;

use crate::replicon_core::{
    NetworkTick, ReplicationBuffer, ReplicationId, ReplicationRules, REPLICATION_CHANNEL_ID,
};
use despawn_tracker::{DespawnTracker, DespawnTrackerPlugin};
use removal_tracker::{RemovalTracker, RemovalTrackerPlugin};

pub const SERVER_ID: u64 = 0;

#[derive(Constructor)]
pub struct ServerPlugin {
    tick_policy: TickPolicy,
}

impl Default for ServerPlugin {
    fn default() -> Self {
        Self {
            tick_policy: TickPolicy::MaxTickRate(30),
        }
    }
}

impl Plugin for ServerPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins((
            RenetServerPlugin,
            NetcodeServerPlugin,
            RemovalTrackerPlugin,
            DespawnTrackerPlugin,
        ))
        .init_resource::<ServerTicks>()
        .configure_set(
            PreUpdate,
            ServerSet::Receive.after(NetcodeServerPlugin::update_system),
        )
        .configure_set(
            PostUpdate,
            ServerSet::Send.before(NetcodeServerPlugin::send_packets),
        )
        .add_systems(
            PreUpdate,
            (Self::acks_receiving_system, Self::acks_cleanup_system)
                .in_set(ServerSet::Receive)
                .run_if(resource_exists::<RenetServer>()),
        )
        .add_systems(
            PostUpdate,
            (
                Self::diffs_sending_system
                    .in_set(ServerSet::Send)
                    .run_if(resource_exists::<RenetServer>()),
                Self::reset_system.run_if(resource_removed::<RenetServer>()),
            ),
        );

        if let TickPolicy::MaxTickRate(max_tick_rate) = self.tick_policy {
            let tick_time = Duration::from_millis(1000 / max_tick_rate as u64);
            app.configure_set(PostUpdate, ServerSet::Send.run_if(on_timer(tick_time)));
        }
    }
}

impl ServerPlugin {
    fn acks_receiving_system(
        mut server_ticks: ResMut<ServerTicks>,
        mut server: ResMut<RenetServer>,
    ) {
        for client_id in server.clients_id() {
            while let Some(message) = server.receive_message(client_id, REPLICATION_CHANNEL_ID) {
                match bincode::deserialize::<NetworkTick>(&message) {
                    Ok(tick) => {
                        let acked_tick = server_ticks.acked_ticks.entry(client_id).or_default();
                        if *acked_tick < tick {
                            *acked_tick = tick;
                        }
                    }
                    Err(e) => error!("unable to deserialize tick from client {client_id}: {e}"),
                }
            }
        }

        server_ticks.cleanup_system_ticks();
    }

    fn acks_cleanup_system(
        mut server_events: EventReader<ServerEvent>,
        mut server_ticks: ResMut<ServerTicks>,
    ) {
        for event in &mut server_events {
            match event {
                ServerEvent::ClientDisconnected { client_id, .. } => {
                    server_ticks.acked_ticks.remove(client_id);
                }
                ServerEvent::ClientConnected { client_id } => {
                    server_ticks.acked_ticks.entry(*client_id).or_default();
                }
            }
        }
    }

    fn diffs_sending_system(
        mut replication_buffers: Local<HashMap<u64, ReplicationBuffer>>,
        change_tick: SystemChangeTick,
        mut set: ParamSet<(&World, ResMut<RenetServer>, ResMut<ServerTicks>)>,
        replication_rules: Res<ReplicationRules>,
        despawn_tracker: Res<DespawnTracker>,
        removal_trackers: Query<(Entity, &RemovalTracker)>,
    ) {
        // remove disconnected clients from replication buffer cache
        {
            let renet_server = set.p1();
            replication_buffers.retain(|client_id, _| renet_server.is_connected(*client_id));
        }

        let mut server_ticks = set.p2();
        server_ticks.increment(change_tick.this_run());

        for (&client_id, &acked_tick) in &server_ticks.acked_ticks {
            let acked_system_tick = *server_ticks
                .system_ticks
                .get(&acked_tick)
                .unwrap_or(&Tick::new(0));
            replication_buffers
                .entry(client_id)
                .or_default()
                .refresh_ticks(server_ticks.current_tick, acked_system_tick);
        }
        collect_changes(
            &mut replication_buffers,
            change_tick.this_run(),
            set.p0(),
            &replication_rules,
        );
        collect_removals(
            &mut replication_buffers,
            change_tick.this_run(),
            &removal_trackers,
        );
        collect_despawns(
            &mut replication_buffers,
            change_tick.this_run(),
            &despawn_tracker,
        );

        for (client_id, replication_buffer) in replication_buffers.iter_mut() {
            let Ok(message) = replication_buffer.consume() else {
                continue;
            };
            set.p1()
                .send_message(*client_id, REPLICATION_CHANNEL_ID, message);
        }
    }

    fn reset_system(mut server_ticks: ResMut<ServerTicks>) {
        server_ticks.acked_ticks.clear();
        server_ticks.system_ticks.clear();
    }
}

fn collect_changes(
    replication_buffers: &mut HashMap<u64, ReplicationBuffer>,
    system_tick: Tick,
    world: &World,
    replication_rules: &ReplicationRules,
) {
    for archetype in world
        .archetypes()
        .iter()
        .filter(|archetype| archetype.id() != ArchetypeId::EMPTY)
        .filter(|archetype| archetype.id() != ArchetypeId::INVALID)
        .filter(|archetype| archetype.contains(replication_rules.replication_id()))
    {
        let table = world
            .storages()
            .tables
            .get(archetype.table_id())
            .expect("archetype should be valid");

        for component_id in archetype.components() {
            let Some(replication_id) = replication_rules.get_id(component_id) else {
                continue;
            };
            let replication_info = replication_rules.get_info(replication_id);
            if archetype.contains(replication_info.ignored_id) {
                continue;
            }

            let storage_type = archetype
                .get_storage_type(component_id)
                .unwrap_or_else(|| panic!("{component_id:?} be in archetype"));

            match storage_type {
                StorageType::Table => {
                    collect_table_components(
                        replication_buffers,
                        replication_rules,
                        system_tick,
                        table,
                        archetype,
                        replication_id,
                        component_id,
                    );
                }
                StorageType::SparseSet => {
                    collect_sparse_set_components(
                        replication_buffers,
                        replication_rules,
                        system_tick,
                        &world.storages().sparse_sets,
                        archetype,
                        replication_id,
                        component_id,
                    );
                }
            }
        }
    }
}

fn collect_table_components(
    replication_buffers: &mut HashMap<u64, ReplicationBuffer>,
    replication_rules: &ReplicationRules,
    system_tick: Tick,
    table: &Table,
    archetype: &Archetype,
    replication_id: ReplicationId,
    component_id: ComponentId,
) {
    let column = table
        .get_column(component_id)
        .unwrap_or_else(|| panic!("{component_id:?} should belong to table"));

    for archetype_entity in archetype.entities() {
        // SAFETY: the table row obtained from the world state.
        let ticks = unsafe { column.get_ticks_unchecked(archetype_entity.table_row()) };
        // SAFETY: component obtained from the archetype.
        let component = unsafe { column.get_data_unchecked(archetype_entity.table_row()) };

        for (_, replication_buffer) in replication_buffers.iter_mut() {
            if ticks.is_changed(replication_buffer.last_acked_system_tick(), system_tick) {
                let _ = replication_buffer.append_updated_component(
                    replication_rules,
                    archetype_entity.entity(),
                    replication_id,
                    component,
                );
            }
        }
    }
}

fn collect_sparse_set_components(
    replication_buffers: &mut HashMap<u64, ReplicationBuffer>,
    replication_rules: &ReplicationRules,
    system_tick: Tick,
    sparse_sets: &SparseSets,
    archetype: &Archetype,
    replication_id: ReplicationId,
    component_id: ComponentId,
) {
    let sparse_set = sparse_sets
        .get(component_id)
        .unwrap_or_else(|| panic!("{component_id:?} should belong to sparse set"));

    for archetype_entity in archetype.entities() {
        let entity = archetype_entity.entity();
        let ticks = sparse_set
            .get_ticks(entity)
            .unwrap_or_else(|| panic!("{entity:?} should have {component_id:?}"));
        let component = sparse_set
            .get(entity)
            .unwrap_or_else(|| panic!("{entity:?} should have {component_id:?}"));

        for (_, replication_buffer) in replication_buffers.iter_mut() {
            if ticks.is_changed(replication_buffer.last_acked_system_tick(), system_tick) {
                let _ = replication_buffer.append_updated_component(
                    replication_rules,
                    entity,
                    replication_id,
                    component,
                );
            }
        }
    }
}

fn collect_removals(
    replication_buffers: &mut HashMap<u64, ReplicationBuffer>,
    system_tick: Tick,
    removal_trackers: &Query<(Entity, &RemovalTracker)>,
) {
    for (entity, removal_tracker) in removal_trackers {
        for (_, replication_buffer) in replication_buffers.iter_mut() {
            for (&replication_id, &tick) in &removal_tracker.0 {
                if tick.is_newer_than(replication_buffer.last_acked_system_tick(), system_tick) {
                    let _ = replication_buffer.append_removed_component(entity, replication_id);
                }
            }
        }
    }
}

fn collect_despawns(
    replication_buffers: &mut HashMap<u64, ReplicationBuffer>,
    system_tick: Tick,
    despawn_tracker: &DespawnTracker,
) {
    for &(entity, tick) in &despawn_tracker.despawns {
        for (_, replication_buffer) in replication_buffers.iter_mut() {
            if tick.is_newer_than(replication_buffer.last_acked_system_tick(), system_tick) {
                let _ = replication_buffer.despawn_entity(entity);
            }
        }
    }
}

/// Condition that returns `true` for server or in singleplayer and `false` for client.
pub fn has_authority() -> impl FnMut(Option<Res<RenetClient>>) -> bool + Clone {
    move |client| client.is_none()
}

/// Set with replication and event systems related to server.
#[derive(SystemSet, Debug, Hash, PartialEq, Eq, Clone, Copy)]
pub enum ServerSet {
    /// Systems that receive data.
    ///
    /// Runs in `PreUpdate`.
    Receive,
    /// Systems that send data.
    ///
    /// Runs in `PostUpdate` on server tick, see [`TickPolicy`].
    Send,
}

pub enum TickPolicy {
    /// Max number of updates sent from server per second. May be lower if update cycle duration is too long.
    ///
    /// By default it's 30 updates per second.
    MaxTickRate(u16),
    /// [`ServerSet::Send`] should be manually configured.
    Manual,
}

/// Stores information about ticks.
///
/// Used only on server.
#[derive(Resource, Default)]
pub struct ServerTicks {
    /// Current server tick.
    current_tick: NetworkTick,

    /// Last acknowledged server ticks for all clients.
    acked_ticks: HashMap<u64, NetworkTick>,

    /// Stores mapping from server ticks to system change ticks.
    system_ticks: HashMap<NetworkTick, Tick>,
}

impl ServerTicks {
    /// Increments current tick by 1 and makes corresponding system tick mapping for it.
    fn increment(&mut self, system_tick: Tick) {
        self.current_tick.increment();
        self.system_ticks.insert(self.current_tick, system_tick);
    }

    /// Removes system tick mappings for acks that was acknowledged by everyone.
    fn cleanup_system_ticks(&mut self) {
        self.system_ticks.retain(|tick, _| {
            self.acked_ticks
                .values()
                .all(|acked_tick| acked_tick > tick)
        })
    }

    /// Returns current server tick.
    pub fn current_tick(&self) -> NetworkTick {
        self.current_tick
    }

    /// Returns last acknowledged server ticks for all clients.
    pub fn acked_ticks(&self) -> &HashMap<u64, NetworkTick> {
        &self.acked_ticks
    }
}
