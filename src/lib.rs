/*!
ECS-focused high-level networking crate for the [Bevy game engine](https://bevyengine.org).

# Quick start

Replicon provides a [`prelude`] module, which exports most of the typically used traits and types.

The library doesn't provide any I/O, so you need to add a messaging backend.
We provide a first-party integration with [`bevy_renet`](https://docs.rs/bevy_renet)
via [`bevy_replicon_renet`](https://docs.rs/bevy_replicon_renet).

If you want to write an integration for a messaging backend,
see the documentation for [`RepliconServer`], [`RepliconClient`] and [`ServerEvent`].
You can also use `bevy_replicon_renet` as a reference.

## Initialization

You need to add [`RepliconPlugins`] and plugins for your chosen messaging backend to your app:

```
use bevy::prelude::*;
use bevy_replicon::prelude::*;
# use bevy::app::PluginGroupBuilder;

let mut app = App::new();
app.add_plugins((MinimalPlugins, RepliconPlugins, MyMessagingPlugins));
#
# struct MyMessagingPlugins;
#
# impl PluginGroup for MyMessagingPlugins {
#     fn build(self) -> PluginGroupBuilder {
#         PluginGroupBuilder::start::<Self>()
#     }
# }
```

If you are planning to separate client and server you can use
[`PluginGroupBuilder::disable()`] to disable [`ClientPlugin`] or [`ServerPlugin`] on [`RepliconPlugins`].
You will need to disable similar plugins on your messaing library of choice too.

You can also configure how often updates are sent from
server to clients with [`ServerPlugin`]'s [`TickPolicy`]:

```
# use bevy::prelude::*;
# use bevy_replicon::prelude::*;
# let mut app = App::new();
app.add_plugins(
    RepliconPlugins
        .build()
        .disable::<ClientPlugin>()
        .set(ServerPlugin {
            tick_policy: TickPolicy::MaxTickRate(60),
            ..Default::default()
        }),
);
```

## Server and client creation

This part is customized based on your messaging backend. For `bevy_replicon_renet`
see [this](https://docs.rs/bevy_replicon_renet#server-and-client-creation) section.

The backend will update the [`RepliconServer`] or [`RepliconClient`] resources, which can be
interacted with without knowing what backend is used. Those resources typically don't need to
be used directly, it is preferred to use more high-level abstractions described later.

Never initialize a client and server in the same app for single-player, it will cause a replication loop.
Use the described pattern in [system sets and conditions](#system-sets-and-conditions)
in combination with [network events](#network-events).

## Component replication

It's a process of sending component changes from server to clients in order to
keep the world in sync.

### Marking for replication

By default, no components are replicated. A component will be replicated if it has been registered for replication
**and** its entity has the [`Replication`] component.

In other words you need two things to start replication:

1. Register component type for replication. Component should implement
[`Serialize`](serde::Serialize) and [`Deserialize`](serde::Deserialize).
You can use [`AppReplicationExt::replicate()`] to register the component for replication:

```
# use bevy::prelude::*;
# use bevy_replicon::prelude::*;
# use serde::{Deserialize, Serialize};
# let mut app = App::new();
# app.add_plugins(RepliconPlugins);
app.replicate::<DummyComponent>();

#[derive(Component, Deserialize, Serialize)]
struct DummyComponent;
```

If your component contains an entity then it cannot be deserialized as is
because entity IDs are different on server and client. The client should do the
mapping. Therefore, to replicate such components properly, they need to implement
the [`MapEntities`](bevy::ecs::entity::MapEntities) trait and register
using [`AppReplicationExt::replicate_mapped()`]:

```
# use bevy::{prelude::*, ecs::entity::{EntityMapper, MapEntities}};
# use bevy_replicon::prelude::*;
# use serde::{Deserialize, Serialize};
#[derive(Component, Deserialize, Serialize)]
struct MappedComponent(Entity);

impl MapEntities for MappedComponent {
    fn map_entities<T: EntityMapper>(&mut self, mapper: &mut T) {
        self.0 = mapper.map_entity(self.0);
    }
}
```

By default all components serialized with [`bincode`] using [`DefaultOptions`](bincode::DefaultOptions).
If your component doesn't implement serde traits or you want to serialize it partially
you can use [`AppReplicationExt::replicate_with`]:

```
use std::io::Cursor;
use bevy::{prelude::*, ptr::Ptr};
use bevy_replicon::{prelude::*, core::replication_rules};
use serde::{Deserialize, Serialize};

# let mut app = App::new();
# app.add_plugins(RepliconPlugins);
app.replicate_with::<Transform>(serialize_transform, deserialize_transform, replication_rules::remove_component::<Transform>);

/// Serializes only translation.
fn serialize_transform(
    component: Ptr,
    cursor: &mut Cursor<Vec<u8>>,
) -> bincode::Result<()> {
    // SAFETY: Function called for registered `ComponentId`.
    let transform: &Transform = unsafe { component.deref() };
    bincode::serialize_into(cursor, &transform.translation)
}

/// Deserializes translation and creates [`Transform`] from it.
fn deserialize_transform(
    entity: &mut EntityWorldMut,
    _entity_map: &mut ServerEntityMap,
    cursor: &mut Cursor<&[u8]>,
    _replicon_tick: RepliconTick,
) -> bincode::Result<()> {
    let translation: Vec3 = bincode::deserialize_from(cursor)?;
    entity.insert(Transform::from_translation(translation));

    Ok(())
}
```

2. You need to choose entities you want to replicate using [`Replication`]
component. Just insert it to the entity you want to replicate. Only components
marked for replication through [`AppReplicationExt::replicate()`]
will be replicated.

If you need to disable replication for a specific component on an entity,
you can call [`CommandDontReplicateExt::dont_replicate::<T>`] on it and replication will be skipped for `T`.

### Tick and fixed timestep games

The [`ServerPlugin`] sends replication data in [`PostUpdate`] any time the [`RepliconTick`] resource
changes. By default, its incremented in [`PostUpdate`] per the [`TickPolicy`].

If you set [`TickPolicy::Manual`], you can increment [`RepliconTick`] at the start of your
game loop inside [`FixedMain`](bevy::app::FixedMain). This value can represent your simulation step, and is made available
to the client in the custom deserialization, despawn and component removal functions.

One use for this is rollback networking: you may want to rollback time and apply the update
for the tick frame, which is in the past, then resimulate.

### Mapping to existing client entities

If you want the server to replicate an entity into a client entity that was already spawned on a client, see [`ClientEntityMap`].

This can be useful for certain types of game. For example, spawning bullets on the client immediately without
waiting on replication.

### "Blueprints" pattern

The idea was borrowed from [iyes_scene_tools](https://github.com/IyesGames/iyes_scene_tools#blueprints-pattern).
You don't want to replicate all components because not all of them are
necessary to send over the network. Components that computed based on other
components (like [`GlobalTransform`]) can be inserted after replication.
This can be easily done using a system with an [`Added`] query filter.
This way, you detect when such entities are spawned into the world, and you can
do any additional setup on them using code. For example, if you have a
character with mesh, you can replicate only your `Player` component and insert
necessary components after replication. If you want to avoid one frame delay, put
your initialization systems to [`ClientSet::Receive`]:

```
# use std::io::Cursor;
# use bevy::{prelude::*, ptr::Ptr};
# use bevy_replicon::{prelude::*, core::replication_rules};
# use serde::{Deserialize, Serialize};
# let mut app = App::new();
# app.add_plugins(RepliconPlugins);
app.replicate_with::<Transform>(serialize_transform, deserialize_transform, replication_rules::remove_component::<Transform>)
    .replicate::<Player>()
    .add_systems(PreUpdate, player_init_system.after(ClientSet::Receive));

fn player_init_system(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<ColorMaterial>>,
    spawned_players: Query<Entity, Added<Player>>,
) {
    for entity in &spawned_players {
        commands.entity(entity).insert((
            GlobalTransform::default(),
            VisibilityBundle::default(),
            meshes.add(Mesh::from(Capsule3d::default())),
            materials.add(Color::AZURE),
        ));
    }
}

#[derive(Component, Deserialize, Serialize)]
struct Player;
# fn serialize_transform(_: Ptr, _: &mut Cursor<Vec<u8>>) -> bincode::Result<()> { unimplemented!() }
# fn deserialize_transform(_: &mut EntityWorldMut, _: &mut ServerEntityMap, _: &mut Cursor<&[u8]>, _: RepliconTick) -> bincode::Result<()> { unimplemented!() }
```

This pairs nicely with server state serialization and keeps saves clean.
You can use [`replicate_into`](scene::replicate_into) to
fill [`DynamicScene`] with replicated entities and their components.

### Component relations

Sometimes components depend on each other. For example, [`Parent`] and
[`Children`] In this case, you can't just replicate the [`Parent`] because you
not only need to add it to the [`Children`] of the parent, but also remove it
from the [`Children`] of the old one. In this case, you need to create a third
component that correctly updates the other two when it changes, and only
replicate that one. This crate provides [`ParentSync`] component that replicates
Bevy hierarchy. For your custom components with relations you need to write your
own with a similar pattern.

## Network events

Network event replace RPCs (remote procedure calls) in other engines and,
unlike components, can be sent both from server to clients and from clients to
server.

### From client to server

To send specific events from client to server, you need to register the event
with [`ClientEventAppExt::add_client_event()`] instead of [`App::add_event()`].
The event must be registered on both the client and the server in the same order.

Events include [`ChannelKind`] to configure delivery guarantees (reliability and
ordering). You can alternatively pass in [`RepliconChannel`] with more advanced configuration.

These events will appear on server as [`FromClient`] wrapper event that
contains sender ID and the sent event. We consider server or a single-player session
also as a client with ID [`ClientId::SERVER`]. So you can send such events even on server
and [`FromClient`] will be emitted for them too. This way your game logic will work the same
on client, listen server and in single-player session.

For systems that receive events attach [`has_authority`] condition to receive a message
on non-client instances (server or single-player):

```
# use bevy::prelude::*;
# use bevy_replicon::prelude::*;
# use serde::{Deserialize, Serialize};
# let mut app = App::new();
# app.add_plugins(RepliconPlugins);
app.add_client_event::<DummyEvent>(ChannelKind::Ordered)
    .add_systems(
        Update,
        (
            event_sending_system,
            event_receiving_system.run_if(has_authority),
        ),
    );

/// Sends an event from client or listen server.
fn event_sending_system(mut dummy_events: EventWriter<DummyEvent>) {
    dummy_events.send_default();
}

/// Receives event on server and single-player.
fn event_receiving_system(mut dummy_events: EventReader<FromClient<DummyEvent>>) {
    for FromClient { client_id, event } in dummy_events.read() {
        info!("received event {event:?} from {client_id:?}");
    }
}

#[derive(Debug, Default, Deserialize, Event, Serialize)]
struct DummyEvent;
```

Just like components, if an event contains an entity, then the client should
map it before sending it to the server.
To do this, use [`ClientEventAppExt::add_mapped_client_event()`] and implement
[`MapEntities`](bevy::ecs::entity::MapEntities):

```
# use bevy::{prelude::*, ecs::entity::MapEntities};
# use bevy_replicon::prelude::*;
# use serde::{Deserialize, Serialize};
# let mut app = App::new();
# app.add_plugins(RepliconPlugins);
app.add_mapped_client_event::<MappedEvent>(ChannelKind::Ordered);

#[derive(Debug, Deserialize, Event, Serialize, Clone)]
struct MappedEvent(Entity);

impl MapEntities for MappedEvent {
    fn map_entities<T: EntityMapper>(&mut self, entity_mapper: &mut T) {
        self.0 = entity_mapper.map_entity(self.0);
    }
}
```

As shown above, mapped client events must also implement [`Clone`].

There is also [`ClientEventAppExt::add_client_event_with()`] to register an event with special sending and receiving functions.
This could be used for sending events that contain [`Box<dyn Reflect>`], which require access to the [`AppTypeRegistry`] resource.
Don't forget to validate the contents of every [`Box<dyn Reflect>`] from a client, it could be anything!

### From server to client

A similar technique is used to send events from server to clients. To do this,
register the event with [`ServerEventAppExt::add_server_event()`] server event
and send it from server using [`ToClients`]. The event must be registered on
both the client and the server in the same order. This wrapper contains send parameters
and the event itself. Just like events sent from the client, you can send these events on the server or
in single-player and they will appear locally as regular events (if [`ClientId::SERVER`] is not excluded
from the send list):

```
# use bevy::prelude::*;
# use bevy_replicon::prelude::*;
# use serde::{Deserialize, Serialize};
# let mut app = App::new();
# app.add_plugins(RepliconPlugins);
app.add_server_event::<DummyEvent>(ChannelKind::Ordered)
    .add_systems(
        Update,
        (
            event_sending_system,
            event_receiving_system.run_if(has_authority),
        ),
    );

/// Sends an event from server or single-player.
fn event_sending_system(mut dummy_events: EventWriter<ToClients<DummyEvent>>) {
    dummy_events.send(ToClients {
        mode: SendMode::Broadcast,
        event: DummyEvent,
    });
}

/// Receives event on client and single-player.
fn event_receiving_system(mut dummy_events: EventReader<DummyEvent>) {
    for event in dummy_events.read() {
        info!("received event {event:?} from server");
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Event, Serialize)]
struct DummyEvent;
```

Just like with client events, if the event contains an entity, then
[`ServerEventAppExt::add_mapped_server_event()`] should be used instead.

For events that require special sending and receiving functions you can use [`ServerEventAppExt::add_server_event_with()`].

## System sets and conditions

When configuring systems for multiplayer game, you often want to run some
systems only on when you have authority over the world simulation
(on server or in single-player session). For example, damage registration or
procedural level generation systems. For this just add [`has_authority`]
condition on such system. If you want your systems to run only on
frames when server send updates to clients use [`ServerSet::Send`].

To check if you are running a server or client, you can use
[`server_running`] and [`client_connected`] conditions.
They rarely used for gameplay systems (since you write the same logic for
multiplayer and single-player!), but could be used for server
creation / connection systems and corresponding UI.

## Client visibility

You can control which parts of the world are visible for each client by setting visibility policy
in [`ServerPlugin`] to [`VisibilityPolicy::Whitelist`] or [`VisibilityPolicy::Blacklist`].

In order to set which entity is visible, you need to use the [`ConnectedClients`] resource
to obtain the [`ConnectedClient`] for a specific client and get its [`ClientVisibility`]:

```
# use bevy::prelude::*;
# use bevy_replicon::prelude::*;
# use serde::{Deserialize, Serialize};
# let mut app = App::new();
app.add_plugins((
    MinimalPlugins,
    RepliconPlugins.set(ServerPlugin {
        visibility_policy: VisibilityPolicy::Whitelist, // Makes all entities invisible for clients by default.
        ..Default::default()
    }),
))
.add_systems(
    Update,
    visibility_system.run_if(server_running),
);

/// Disables the visibility of other players' entities that are further away than the visible distance.
fn visibility_system(
    mut connected_clients: ResMut<ConnectedClients>,
    moved_players: Query<(&Transform, &Player), Changed<Transform>>,
    other_players: Query<(Entity, &Transform, &Player)>,
) {
    for (moved_transform, moved_player) in &moved_players {
        let client = connected_clients.client_mut(moved_player.0);
        for (entity, transform, _) in other_players
            .iter()
            .filter(|(.., player)| player.0 != moved_player.0)
        {
            const VISIBLE_DISTANCE: f32 = 100.0;
            let distance = moved_transform.translation.distance(transform.translation);
            client
                .visibility_mut()
                .set_visibility(entity, distance < VISIBLE_DISTANCE);
        }
    }
}

#[derive(Component, Deserialize, Serialize)]
struct Player(ClientId);
```

For a higher level API consider using [`bevy_replicon_attributes`](https://docs.rs/bevy_replicon_attributes).

## Eventual consistency

All events, inserts, removals and despawns will be applied to clients in the same order as on the server.

Entity component updates are grouped by entity, and component groupings may be applied to clients in a different order than on the server.
For example, if two entities are spawned in tick 1 on the server and their components are updated in tick 2,
then the client is guaranteed to see the spawns at the same time, but the component updates may appear in different client ticks.

If a component is dependent on other data, updates to the component will only be applied to the client when that data has arrived.
So if your component references another entity, updates to that component will only be applied when the referenced entity has been spawned on the client.

Updates for despawned entities will be discarded automatically, but events or components may reference despawned entities and should be handled with that in mind.

Clients should never assume their world state is the same as the server's on any given tick value-wise.
World state on the client is only "eventually consistent" with the server's.

## Limits

To reduce packet size there are the following limits per replication update:

- Up to [`u16::MAX`] entities that have added components with up to [`u16::MAX`] bytes of component data.
- Up to [`u16::MAX`] entities that have changed components with up to [`u16::MAX`] bytes of component data.
- Up to [`u16::MAX`] entities that have removed components with up to [`u16::MAX`] bytes of component data.
- Up to [`u16::MAX`] entities that were despawned.
*/

pub mod client;
pub mod core;
pub mod network_event;
pub mod parent_sync;
pub mod scene;
pub mod server;
pub mod test_app;

pub mod prelude {
    pub use super::{
        client::{
            client_mapper::{ClientMapper, ServerEntityMap},
            diagnostics::{ClientDiagnosticsPlugin, ClientStats},
            replicon_client::{RepliconClient, RepliconClientStatus},
            BufferedUpdates, ClientPlugin, ClientSet, ServerEntityTicks,
        },
        core::{
            common_conditions::*,
            dont_replicate::{CommandDontReplicateExt, EntityDontReplicateExt},
            replication_rules::{AppReplicationExt, Replication, ReplicationRules},
            replicon_channels::{
                ChannelKind, ReplicationChannel, RepliconChannel, RepliconChannels,
            },
            replicon_tick::RepliconTick,
            ClientId, RepliconCorePlugin,
        },
        network_event::{
            client_event::{ClientEventAppExt, ClientEventChannel, FromClient},
            server_event::{
                SendMode, ServerEventAppExt, ServerEventChannel, ServerEventQueue, ToClients,
            },
            EventMapper,
        },
        parent_sync::{ParentSync, ParentSyncPlugin},
        server::{
            connected_clients::{
                client_visibility::ClientVisibility, ConnectedClient, ConnectedClients,
            },
            replicon_server::RepliconServer,
            ClientEntityMap, ClientMapping, ServerEvent, ServerPlugin, ServerSet, TickPolicy,
            VisibilityPolicy,
        },
        RepliconPlugins,
    };
}

pub use bincode;

use bevy::{app::PluginGroupBuilder, prelude::*};
use prelude::*;

/// Plugin Group for all replicon plugins.
pub struct RepliconPlugins;

impl PluginGroup for RepliconPlugins {
    fn build(self) -> PluginGroupBuilder {
        PluginGroupBuilder::start::<Self>()
            .add(RepliconCorePlugin)
            .add(ParentSyncPlugin)
            .add(ClientPlugin)
            .add(ServerPlugin::default())
    }
}
