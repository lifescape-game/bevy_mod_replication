use std::io::Cursor;

use bevy::{
    ecs::{component::ComponentId, entity::MapEntities},
    prelude::*,
    ptr::Ptr,
    utils::HashMap,
};
use bincode::{DefaultOptions, Options};
use serde::{de::DeserializeOwned, Deserialize, Serialize};

use super::{dont_replicate::DontReplicate, replicon_tick::RepliconTick};
use crate::client::client_mapper::{ClientMapper, ServerEntityMap};

pub trait AppReplicationExt {
    /// Marks component for replication.
    ///
    /// Component will be serialized as is using bincode.
    fn replicate<C>(&mut self) -> &mut Self
    where
        C: Component + Serialize + DeserializeOwned;

    /// Same as [`Self::replicate`], but maps component entities using [`MapNetworkEntities`] trait.
    ///
    /// Always use it for components that contains entities.
    fn replicate_mapped<C>(&mut self) -> &mut Self
    where
        C: Component + Serialize + DeserializeOwned + MapEntities;

    /// Same as [`Self::replicate`], but uses the specified functions for serialization, deserialization, and removal.
    fn replicate_with<C>(
        &mut self,
        serialize: SerializeFn,
        deserialize: DeserializeFn,
        remove: RemoveComponentFn,
    ) -> &mut Self
    where
        C: Component;
}

impl AppReplicationExt for App {
    fn replicate<C>(&mut self) -> &mut Self
    where
        C: Component + Serialize + DeserializeOwned,
    {
        self.replicate_with::<C>(
            serialize_component::<C>,
            deserialize_component::<C>,
            remove_component::<C>,
        )
    }

    fn replicate_mapped<C>(&mut self) -> &mut Self
    where
        C: Component + Serialize + DeserializeOwned + MapEntities,
    {
        self.replicate_with::<C>(
            serialize_component::<C>,
            deserialize_mapped_component::<C>,
            remove_component::<C>,
        )
    }

    fn replicate_with<C>(
        &mut self,
        serialize: SerializeFn,
        deserialize: DeserializeFn,
        remove: RemoveComponentFn,
    ) -> &mut Self
    where
        C: Component,
    {
        let component_id = self.world.init_component::<C>();
        let dont_replicate_id = self.world.init_component::<DontReplicate<C>>();
        let replicated_component = ReplicationInfo {
            dont_replicate_id,
            serialize,
            deserialize,
            remove,
        };

        let mut replication_rules = self.world.resource_mut::<ReplicationRules>();
        replication_rules.info.push(replicated_component);

        let replication_id = ReplicationId(replication_rules.info.len() - 1);
        replication_rules.ids.insert(component_id, replication_id);

        self
    }
}

/// Stores information about which components will be serialized and how.
#[derive(Resource)]
pub struct ReplicationRules {
    /// Custom function to handle entity despawning.
    ///
    /// By default uses [`despawn_recursive`].
    /// Useful if you need to intercept despawns and handle them in a special way.
    pub despawn_fn: EntityDespawnFn,

    /// Maps component IDs to their replication IDs.
    ids: HashMap<ComponentId, ReplicationId>,

    /// Meta information about components that should be replicated.
    info: Vec<ReplicationInfo>,

    /// ID of [`Replication`] component.
    marker_id: ComponentId,
}

impl ReplicationRules {
    /// ID of [`Replication`] component.
    pub(crate) fn get_marker_id(&self) -> ComponentId {
        self.marker_id
    }

    /// Returns mapping of replicated components to their replication IDs.
    pub(crate) fn get_ids(&self) -> &HashMap<ComponentId, ReplicationId> {
        &self.ids
    }

    /// Returns replication ID and meta information about the component if it's replicated.
    pub(crate) fn get(
        &self,
        component_id: ComponentId,
    ) -> Option<(ReplicationId, &ReplicationInfo)> {
        let replication_id = self.ids.get(&component_id).copied()?;
        let replication_info = &self.info[replication_id.0];

        Some((replication_id, replication_info))
    }

    /// Returns meta information about replicated component.
    ///
    /// # Safety
    ///
    /// `replication_id` should come from the same replication rules.
    pub(crate) unsafe fn get_info_unchecked(
        &self,
        replication_id: ReplicationId,
    ) -> &ReplicationInfo {
        self.info.get_unchecked(replication_id.0)
    }
}

impl FromWorld for ReplicationRules {
    fn from_world(world: &mut World) -> Self {
        Self {
            info: Default::default(),
            ids: Default::default(),
            marker_id: world.init_component::<Replication>(),
            despawn_fn: despawn_recursive,
        }
    }
}

/// Signature of component serialization functions.
pub type SerializeFn = fn(Ptr, &mut Cursor<Vec<u8>>) -> bincode::Result<()>;

/// Signature of component deserialization functions.
pub type DeserializeFn = fn(
    &mut EntityWorldMut,
    &mut ServerEntityMap,
    &mut Cursor<&[u8]>,
    RepliconTick,
) -> bincode::Result<()>;

/// Signature of component removal functions.
pub type RemoveComponentFn = fn(&mut EntityWorldMut, RepliconTick);

/// Signature of the entity despawn function.
pub type EntityDespawnFn = fn(EntityWorldMut, RepliconTick);

/// Stores meta information about replicated component.
#[derive(Clone)]
pub(crate) struct ReplicationInfo {
    /// ID of [`DontReplicate<T>`] component.
    pub(crate) dont_replicate_id: ComponentId,

    /// Function that serializes component into bytes.
    pub(crate) serialize: SerializeFn,

    /// Function that deserializes component from bytes and inserts it to [`EntityWorldMut`].
    pub(crate) deserialize: DeserializeFn,

    /// Function that removes specific component from [`EntityWorldMut`].
    pub(crate) remove: RemoveComponentFn,
}

/// Marks entity for replication.
#[derive(Component, Clone, Copy, Default, Reflect, Debug)]
#[reflect(Component)]
pub struct Replication;

/// Same as [`ComponentId`], but consistent between server and clients.
///
/// Internally represents index of [`ReplicationInfo`].
#[derive(Clone, Copy, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub(crate) struct ReplicationId(usize);

/// Default serialization function.
pub fn serialize_component<C: Component + Serialize>(
    component: Ptr,
    cursor: &mut Cursor<Vec<u8>>,
) -> bincode::Result<()> {
    // SAFETY: Function called for registered `ComponentId`.
    let component: &C = unsafe { component.deref() };
    DefaultOptions::new().serialize_into(cursor, component)
}

/// Default deserialization function.
pub fn deserialize_component<C: Component + DeserializeOwned>(
    entity: &mut EntityWorldMut,
    _entity_map: &mut ServerEntityMap,
    cursor: &mut Cursor<&[u8]>,
    _replicon_tick: RepliconTick,
) -> bincode::Result<()> {
    let component: C = DefaultOptions::new().deserialize_from(cursor)?;
    entity.insert(component);

    Ok(())
}

/// Like [`deserialize_component`], but also maps entities before insertion.
pub fn deserialize_mapped_component<C: Component + DeserializeOwned + MapEntities>(
    entity: &mut EntityWorldMut,
    entity_map: &mut ServerEntityMap,
    cursor: &mut Cursor<&[u8]>,
    _replicon_tick: RepliconTick,
) -> bincode::Result<()> {
    let mut component: C = DefaultOptions::new().deserialize_from(cursor)?;

    entity.world_scope(|world| {
        component.map_entities(&mut ClientMapper::new(world, entity_map));
    });

    entity.insert(component);

    Ok(())
}

/// Default component removal function.
pub fn remove_component<C: Component>(entity: &mut EntityWorldMut, _replicon_tick: RepliconTick) {
    entity.remove::<C>();
}

/// Default entity despawn function.
pub fn despawn_recursive(entity: EntityWorldMut, _replicon_tick: RepliconTick) {
    entity.despawn_recursive();
}
