use std::fmt::Debug;

use bevy::{
    ecs::{entity::MapEntities, event::Event},
    prelude::*,
};
use bevy_renet::renet::{RenetClient, RenetServer};
use bincode::{DefaultOptions, Options};
use serde::{
    de::{DeserializeOwned, DeserializeSeed},
    Serialize,
};

use super::{BuildEventDeserializer, BuildEventSerializer, EventChannel};
use crate::{
    client::{ClientState, NetworkEntityMap},
    prelude::NetworkChannels,
    server::{ServerSet, ServerState, SERVER_ID},
};

/// An extension trait for [`App`] for creating client events.
pub trait ClientEventAppExt {
    /// Registers [`FromClient<T>`] event that will be emitted on server after sending `T` event on client.
    fn add_client_event<T: Event + Serialize + DeserializeOwned + Debug>(&mut self) -> &mut Self;

    /// Same as [`Self::add_client_event`], but additionally maps client entities to server before sending.
    fn add_mapped_client_event<T: Event + Serialize + DeserializeOwned + Debug + MapEntities>(
        &mut self,
    ) -> &mut Self;

    /// Same as [`Self::add_client_event`], but the event will be serialized/deserialized using `S`/`D`
    /// with access to [`AppTypeRegistry`].
    ///
    /// Needed to send events that contain things like `Box<dyn Reflect>`.
    fn add_client_reflect_event<T, S, D>(&mut self) -> &mut Self
    where
        T: Event + Debug,
        S: BuildEventSerializer<T> + 'static,
        D: BuildEventDeserializer + 'static,
        for<'a> S::EventSerializer<'a>: Serialize,
        for<'a, 'de> D::EventDeserializer<'a>: DeserializeSeed<'de, Value = T>;

    /// Same as [`Self::add_client_reflect_event`], but additionally maps client entities to server before sending.
    fn add_mapped_client_reflect_event<T, S, D>(&mut self) -> &mut Self
    where
        T: Event + Debug + MapEntities,
        S: BuildEventSerializer<T> + 'static,
        D: BuildEventDeserializer + 'static,
        for<'a> S::EventSerializer<'a>: Serialize,
        for<'a, 'de> D::EventDeserializer<'a>: DeserializeSeed<'de, Value = T>;

    /// Same as [`Self::add_client_event`], but uses specified sending and receiving systems.
    fn add_client_event_with<T: Event + Debug, Marker1, Marker2>(
        &mut self,
        sending_system: impl IntoSystemConfig<Marker1>,
        receiving_system: impl IntoSystemConfig<Marker2>,
    ) -> &mut Self;
}

impl ClientEventAppExt for App {
    fn add_client_event<T: Event + Serialize + DeserializeOwned + Debug>(&mut self) -> &mut Self {
        self.add_client_event_with::<T, _, _>(sending_system::<T>, receiving_system::<T>)
    }

    fn add_mapped_client_event<T: Event + Serialize + DeserializeOwned + Debug + MapEntities>(
        &mut self,
    ) -> &mut Self {
        self.add_client_event_with::<T, _, _>(
            mapping_and_sending_system::<T>,
            receiving_system::<T>,
        )
    }

    fn add_client_reflect_event<T, S, D>(&mut self) -> &mut Self
    where
        T: Event + Debug,
        S: BuildEventSerializer<T> + 'static,
        D: BuildEventDeserializer + 'static,
        for<'a> S::EventSerializer<'a>: Serialize,
        for<'a, 'de> D::EventDeserializer<'a>: DeserializeSeed<'de, Value = T>,
    {
        self.add_client_event_with::<T, _, _>(
            sending_reflect_system::<T, S>,
            receiving_reflect_system::<T, D>,
        )
    }

    fn add_mapped_client_reflect_event<T, S, D>(&mut self) -> &mut Self
    where
        T: Event + Debug + MapEntities,
        S: BuildEventSerializer<T> + 'static,
        D: BuildEventDeserializer + 'static,
        for<'a> S::EventSerializer<'a>: Serialize,
        for<'a, 'de> D::EventDeserializer<'a>: DeserializeSeed<'de, Value = T>,
    {
        self.add_client_event_with::<T, _, _>(
            mapping_and_sending_reflect_system::<T, S>,
            receiving_reflect_system::<T, D>,
        )
    }

    fn add_client_event_with<T: Event + Debug, Marker1, Marker2>(
        &mut self,
        sending_system: impl IntoSystemConfig<Marker1>,
        receiving_system: impl IntoSystemConfig<Marker2>,
    ) -> &mut Self {
        let channel_id = self
            .world
            .resource_mut::<NetworkChannels>()
            .create_client_channel();

        self.add_event::<T>()
            .add_event::<FromClient<T>>()
            .insert_resource(EventChannel::<T>::new(channel_id))
            .add_system(sending_system.in_set(ServerSet::SendEvents).run_if(
                resource_exists::<State<ClientState>>().and_then(in_state(ClientState::Connected)),
            ))
            .add_system(local_resending_system::<T>.in_set(ServerSet::Authority))
            .add_system(receiving_system.in_set(ServerSet::ReceiveEvents).run_if(
                resource_exists::<State<ServerState>>().and_then(in_state(ServerState::Hosting)),
            ));

        self
    }
}

fn sending_system<T: Event + Serialize + Debug>(
    mut events: EventReader<T>,
    mut client: ResMut<RenetClient>,
    channel: Res<EventChannel<T>>,
) {
    for event in &mut events {
        let message = bincode::serialize(&event).expect("client event should be serializable");
        client.send_message(channel.id, message);
        debug!("sent client event {event:?}");
    }
}

fn mapping_and_sending_system<T: Event + MapEntities + Serialize + Debug>(
    mut events: ResMut<Events<T>>,
    mut client: ResMut<RenetClient>,
    entity_map: Res<NetworkEntityMap>,
    channel: Res<EventChannel<T>>,
) {
    for mut event in events.drain() {
        event
            .map_entities(entity_map.to_server())
            .unwrap_or_else(|e| panic!("client event {event:?} should map its entities: {e}"));
        let message =
            bincode::serialize(&event).expect("mapped client event should be serializable");
        client.send_message(channel.id, message);
        debug!("sent mapped client event {event:?}");
    }
}

fn sending_reflect_system<T, S>(
    mut events: EventReader<T>,
    mut client: ResMut<RenetClient>,
    channel: Res<EventChannel<T>>,
    registry: Res<AppTypeRegistry>,
) where
    T: Event + Debug,
    S: BuildEventSerializer<T>,
    for<'a> S::EventSerializer<'a>: Serialize,
{
    let registry = registry.read();
    for event in &mut events {
        let serializer = S::new(event, &registry);
        let message =
            bincode::serialize(&serializer).expect("client reflect event should be serializable");
        client.send_message(channel.id, message);
        debug!("sent client reflect event {event:?}");
    }
}

fn mapping_and_sending_reflect_system<T, S>(
    mut events: ResMut<Events<T>>,
    mut client: ResMut<RenetClient>,
    entity_map: Res<NetworkEntityMap>,
    channel: Res<EventChannel<T>>,
    registry: Res<AppTypeRegistry>,
) where
    T: Event + MapEntities + Debug,
    S: BuildEventSerializer<T>,
    for<'a> S::EventSerializer<'a>: Serialize,
{
    let registry = registry.read();
    for mut event in events.drain() {
        event
            .map_entities(entity_map.to_server())
            .unwrap_or_else(|e| {
                panic!("client reflect event {event:?} should map its entities: {e}")
            });
        let serializer = S::new(&event, &registry);
        let message = bincode::serialize(&serializer)
            .expect("mapped client reflect event should be serializable");
        client.send_message(channel.id, message);
        debug!("sent mapped client reflect event {event:?}");
    }
}

/// Transforms [`T`] events into [`FromClient<T>`] events to "emulate"
/// message sending for offline mode or when server is also a player
fn local_resending_system<T: Event + Debug>(
    mut events: ResMut<Events<T>>,
    mut client_events: EventWriter<FromClient<T>>,
) {
    for event in events.drain() {
        debug!("converted client event {event:?} into a local");
        client_events.send(FromClient {
            client_id: SERVER_ID,
            event,
        })
    }
}

fn receiving_system<T: Event + DeserializeOwned + Debug>(
    mut client_events: EventWriter<FromClient<T>>,
    mut server: ResMut<RenetServer>,
    channel: Res<EventChannel<T>>,
) {
    for client_id in server.clients_id() {
        while let Some(message) = server.receive_message(client_id, channel.id) {
            match bincode::deserialize(&message) {
                Ok(event) => {
                    debug!("received event {event:?} from client {client_id}");
                    client_events.send(FromClient { client_id, event });
                }
                Err(e) => error!("unable to deserialize event from client {client_id}: {e}"),
            }
        }
    }
}

fn receiving_reflect_system<T, D>(
    mut client_events: EventWriter<FromClient<T>>,
    mut server: ResMut<RenetServer>,
    channel: Res<EventChannel<T>>,
    registry: Res<AppTypeRegistry>,
) where
    T: Event + Debug,
    D: BuildEventDeserializer,
    for<'a, 'de> D::EventDeserializer<'a>: DeserializeSeed<'de, Value = T>,
{
    let registry = registry.read();
    for client_id in server.clients_id() {
        while let Some(message) = server.receive_message(client_id, channel.id) {
            // Set options to match `bincode::serialize`.
            // https://docs.rs/bincode/latest/bincode/config/index.html#options-struct-vs-bincode-functions
            let options = DefaultOptions::new()
                .with_fixint_encoding()
                .allow_trailing_bytes();
            let mut deserializer = bincode::Deserializer::from_slice(&message, options);
            match D::new(&registry).deserialize(&mut deserializer) {
                Ok(event) => {
                    debug!("received reflect event {event:?} from client {client_id}");
                    client_events.send(FromClient { client_id, event });
                }
                Err(e) => {
                    error!("unable to deserialize reflect event from client {client_id}: {e}")
                }
            }
        }
    }
}

/// An event indicating that a message from client was received.
/// Emited only on server.
#[derive(Clone, Copy)]
pub struct FromClient<T> {
    pub client_id: u64,
    pub event: T,
}

#[cfg(test)]
mod tests {
    use bevy::ecs::event::Events;

    use super::*;
    use crate::{
        network_event::test_events::{
            DummyEvent, ReflectEvent, ReflectEventDeserializer, ReflectEventSerializer,
        },
        test_network::TestNetworkPlugin,
        ClientPlugin, ReplicationPlugins, ServerPlugin,
    };

    #[test]
    fn without_server_plugin() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .add_plugins(ReplicationPlugins.build().disable::<ServerPlugin>())
            .add_client_event_with::<DummyEvent, _, _>(|| {}, || {})
            .update();
    }

    #[test]
    fn without_client_plugin() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .add_plugins(ReplicationPlugins.build().disable::<ClientPlugin>())
            .add_client_event_with::<DummyEvent, _, _>(|| {}, || {})
            .update();
    }

    #[test]
    fn sending_receiving() {
        let mut app = App::new();
        app.add_plugins(ReplicationPlugins)
            .add_client_event::<DummyEvent>()
            .add_plugin(TestNetworkPlugin);

        app.world
            .resource_mut::<Events<DummyEvent>>()
            .send(DummyEvent(Entity::PLACEHOLDER));

        app.update();
        app.update();

        let client_events = app.world.resource::<Events<FromClient<DummyEvent>>>();
        assert_eq!(client_events.len(), 1);
    }

    #[test]
    fn mapping_and_sending_receiving() {
        let mut app = App::new();
        app.add_plugins(ReplicationPlugins)
            .add_mapped_client_event::<DummyEvent>()
            .add_plugin(TestNetworkPlugin);

        let client_entity = Entity::from_raw(0);
        let server_entity = Entity::from_raw(client_entity.index() + 1);
        app.world
            .resource_mut::<NetworkEntityMap>()
            .insert(server_entity, client_entity);

        app.world
            .resource_mut::<Events<DummyEvent>>()
            .send(DummyEvent(client_entity));

        app.update();
        app.update();

        let mapped_entities: Vec<_> = app
            .world
            .resource_mut::<Events<FromClient<DummyEvent>>>()
            .drain()
            .map(|event| event.event.0)
            .collect();
        assert_eq!(mapped_entities, [server_entity]);
    }

    #[test]
    fn sending_receiving_reflect() {
        let mut app = App::new();
        app.add_plugins(ReplicationPlugins)
            .register_type::<DummyComponent>()
            .add_client_reflect_event::<ReflectEvent, ReflectEventSerializer, ReflectEventDeserializer>()
            .add_plugin(TestNetworkPlugin);

        app.world
            .resource_mut::<Events<ReflectEvent>>()
            .send(ReflectEvent {
                entity: Entity::PLACEHOLDER,
                component: DummyComponent.clone_value(),
            });

        app.update();
        app.update();

        let client_events = app.world.resource::<Events<FromClient<ReflectEvent>>>();
        assert_eq!(client_events.len(), 1);
    }

    #[test]
    fn mapping_and_sending_receiving_reflect() {
        let mut app = App::new();
        app.add_plugins(ReplicationPlugins)
            .register_type::<DummyComponent>()
            .add_mapped_client_reflect_event::<ReflectEvent, ReflectEventSerializer, ReflectEventDeserializer>()
            .add_plugin(TestNetworkPlugin);

        let client_entity = Entity::from_raw(0);
        let server_entity = Entity::from_raw(client_entity.index() + 1);
        app.world
            .resource_mut::<NetworkEntityMap>()
            .insert(server_entity, client_entity);

        app.world
            .resource_mut::<Events<ReflectEvent>>()
            .send(ReflectEvent {
                entity: client_entity,
                component: DummyComponent.clone_value(),
            });

        app.update();
        app.update();

        let mapped_entities: Vec<_> = app
            .world
            .resource_mut::<Events<FromClient<ReflectEvent>>>()
            .drain()
            .map(|event| event.event.entity)
            .collect();
        assert_eq!(mapped_entities, [server_entity]);
    }

    #[test]
    fn local_resending() {
        let mut app = App::new();
        app.add_plugins(ReplicationPlugins)
            .add_client_event::<DummyEvent>();

        app.world
            .resource_mut::<Events<DummyEvent>>()
            .send(DummyEvent(Entity::PLACEHOLDER));

        app.update();

        let dummy_events = app.world.resource::<Events<DummyEvent>>();
        assert!(dummy_events.is_empty());

        let client_events = app.world.resource::<Events<FromClient<DummyEvent>>>();
        assert_eq!(client_events.len(), 1);
    }

    #[derive(Reflect)]
    struct DummyComponent;
}
