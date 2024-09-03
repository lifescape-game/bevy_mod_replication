use std::{
    any::{self, TypeId},
    io::Cursor,
    mem,
};

use bevy::{
    ecs::{
        component::{ComponentId, Components},
        entity::MapEntities,
        event::ManualEventReader,
    },
    prelude::*,
    ptr::{Ptr, PtrMut},
};
use bincode::{DefaultOptions, Options};
use serde::{de::DeserializeOwned, Serialize};

use super::EventRegistry;
use crate::core::{
    channels::{RepliconChannel, RepliconChannels},
    ctx::{ClientSendCtx, ServerReceiveCtx},
    replicon_client::RepliconClient,
    replicon_server::RepliconServer,
    ClientId,
};

/// An extension trait for [`App`] for creating client events.
pub trait ClientEventAppExt {
    /// Registers [`FromClient<E>`] and `E` events.
    ///
    /// [`FromClient<E>`] will be emitted on the server after sending `E` event on client.
    /// In listen-server mode `E` will be drained right after sending and re-emitted as
    /// [`FromClient<E>`] with [`ClientId::SERVER`](crate::core::ClientId::SERVER).
    ///
    /// Can be called for events that were registered with [add_event](bevy::app::App::add_event).
    /// A duplicate registration for `E` won't be created.
    /// But be careful, since on listen servers all events `E` are drained,
    /// which could break other Bevy or third-party plugin systems that listen for `E`.
    ///
    /// See also [`Self::add_client_event_with`] and the [corresponding section](../index.html#from-client-to-server)
    /// from the quick start guide.
    fn add_client_event<E: Event + Serialize + DeserializeOwned>(
        &mut self,
        channel: impl Into<RepliconChannel>,
    ) -> &mut Self {
        self.add_client_event_with(channel, default_serialize::<E>, default_deserialize::<E>)
    }

    /// Same as [`Self::add_client_event`], but additionally maps client entities to server inside the event before sending.
    ///
    /// Always use it for events that contain entities.
    /// See also [`Self::add_client_event`].
    fn add_mapped_client_event<E: Event + Serialize + DeserializeOwned + MapEntities + Clone>(
        &mut self,
        channel: impl Into<RepliconChannel>,
    ) -> &mut Self {
        self.add_client_event_with(
            channel,
            default_serialize_mapped::<E>,
            default_deserialize::<E>,
        )
    }

    /**
    Same as [`Self::add_client_event`], but uses the specified functions for serialization and deserialization.

    # Examples

    Register an event with [`Box<dyn Reflect>`]:

    ```
    use std::io::Cursor;

    use bevy::{
        prelude::*,
        reflect::serde::{ReflectSerializer, ReflectDeserializer},
    };
    use bevy_replicon::{
        core::ctx::{ClientSendCtx, ServerReceiveCtx},
        prelude::*,
    };
    use bincode::{DefaultOptions, Options};
    use serde::de::DeserializeSeed;

    let mut app = App::new();
    app.add_plugins((MinimalPlugins, RepliconPlugins));
    app.add_client_event_with(
        ChannelKind::Ordered,
        serialize_reflect,
        deserialize_reflect,
    );

    fn serialize_reflect(
        ctx: &mut ClientSendCtx,
        event: &ReflectEvent,
        cursor: &mut Cursor<Vec<u8>>,
    ) -> bincode::Result<()> {
        let serializer = ReflectSerializer::new(&*event.0, ctx.registry);
        DefaultOptions::new().serialize_into(cursor, &serializer)
    }

    fn deserialize_reflect(
        ctx: &mut ServerReceiveCtx,
        cursor: &mut Cursor<&[u8]>,
    ) -> bincode::Result<ReflectEvent> {
        let mut deserializer = bincode::Deserializer::with_reader(cursor, DefaultOptions::new());
        let reflect = ReflectDeserializer::new(ctx.registry).deserialize(&mut deserializer)?;
        Ok(ReflectEvent(reflect))
    }

    #[derive(Event)]
    struct ReflectEvent(Box<dyn Reflect>);
    ```
    */
    fn add_client_event_with<E: Event>(
        &mut self,
        channel: impl Into<RepliconChannel>,
        serialize: SerializeFn<E>,
        deserialize: DeserializeFn<E>,
    ) -> &mut Self;
}

impl ClientEventAppExt for App {
    fn add_client_event_with<E: Event>(
        &mut self,
        channel: impl Into<RepliconChannel>,
        serialize: SerializeFn<E>,
        deserialize: DeserializeFn<E>,
    ) -> &mut Self {
        debug!("registering client event `{}`", any::type_name::<E>());

        self.add_event::<E>()
            .add_event::<FromClient<E>>()
            .init_resource::<ClientEventReader<E>>();

        let channel_id = self
            .world_mut()
            .resource_mut::<RepliconChannels>()
            .create_client_channel(channel.into());

        self.world_mut()
            .resource_scope(|world, mut event_registry: Mut<EventRegistry>| {
                event_registry.register_client_event(ClientEvent::new(
                    world.components(),
                    channel_id,
                    serialize,
                    deserialize,
                ));
            });

        self
    }
}

/// Type-erased functions and metadata for a registered client event.
///
/// Needed so events of different types can be processed together.
pub(crate) struct ClientEvent {
    type_id: TypeId,
    type_name: &'static str,

    /// ID of [`Events<E>`] resource.
    events_id: ComponentId,

    /// ID of [`ClientEventReader<E>`] resource.
    reader_id: ComponentId,

    /// ID of [`Events<ToClients<E>>`] resource.
    client_events_id: ComponentId,

    /// Used channel.
    channel_id: u8,

    send: SendFn,
    receive: ReceiveFn,
    resend_locally: ResendLocallyFn,
    reset: ResetFn,
    serialize: unsafe fn(),
    deserialize: unsafe fn(),
}

impl ClientEvent {
    fn new<E: Event>(
        components: &Components,
        channel_id: u8,
        serialize: SerializeFn<E>,
        deserialize: DeserializeFn<E>,
    ) -> Self {
        let events_id = components.resource_id::<Events<E>>().unwrap_or_else(|| {
            panic!(
                "event `{}` should be previously registered",
                any::type_name::<E>()
            )
        });
        let client_events_id = components
            .resource_id::<Events<FromClient<E>>>()
            .unwrap_or_else(|| {
                panic!(
                    "event `{}` should be previously registered",
                    any::type_name::<FromClient<E>>()
                )
            });
        let reader_id = components
            .resource_id::<ClientEventReader<E>>()
            .unwrap_or_else(|| {
                panic!(
                    "resource `{}` should be previously inserted",
                    any::type_name::<ClientEventReader<E>>()
                )
            });

        // SAFETY: these functions won't be called until the type is restored.
        Self {
            type_id: TypeId::of::<E>(),
            type_name: any::type_name::<E>(),
            events_id,
            reader_id,
            client_events_id,
            channel_id,
            send: send::<E>,
            receive: receive::<E>,
            resend_locally: resend_locally::<E>,
            reset: reset::<E>,
            serialize: unsafe { mem::transmute::<SerializeFn<E>, unsafe fn()>(serialize) },
            deserialize: unsafe { mem::transmute::<DeserializeFn<E>, unsafe fn()>(deserialize) },
        }
    }

    pub(crate) fn events_id(&self) -> ComponentId {
        self.events_id
    }

    pub(crate) fn reader_id(&self) -> ComponentId {
        self.reader_id
    }

    pub(crate) fn client_events_id(&self) -> ComponentId {
        self.client_events_id
    }

    /// Sends an event to the server.
    ///
    /// # Safety
    ///
    /// The caller must ensure that `events` is [`Events<E>`], `reader` is [`ClientEventReader<E>`]
    /// and this instance was created for `E`.
    pub(crate) unsafe fn send(
        &self,
        ctx: &mut ClientSendCtx,
        events: &Ptr,
        reader: PtrMut,
        client: &mut RepliconClient,
    ) {
        (self.send)(self, ctx, events, reader, client);
    }

    /// Receives an event from a client.
    ///
    /// # Safety
    ///
    /// The caller must ensure that `events` is [`Events<FromClient<E>>`]
    /// and this instance was created for `E`.
    pub(crate) unsafe fn receive(
        &self,
        ctx: &mut ServerReceiveCtx,
        client_events: PtrMut,
        server: &mut RepliconServer,
    ) {
        (self.receive)(self, ctx, client_events, server);
    }

    /// Drains events `E` and re-emits them as [`FromClient<E>`].
    ///
    /// # Safety
    ///
    /// The caller must ensure that `events` is [`Events<E>`], `client_events` is [`Events<FromClient<E>>`]
    /// and this instance was created for `E`.
    pub(crate) unsafe fn resend_locally(&self, client_events: PtrMut, events: PtrMut) {
        (self.resend_locally)(client_events, events);
    }

    /// Drains all events.
    ///
    /// # Safety
    ///
    /// The caller must ensure that `events` is [`Events<E>`]
    /// and this instance was created for `E`.
    pub(crate) unsafe fn reset(&self, events: PtrMut) {
        (self.reset)(events);
    }

    /// Serializes an event into a cursor.
    ///
    /// # Safety
    ///
    /// The caller must ensure that this instance was created for `E`.
    unsafe fn serialize<E: Event>(
        &self,
        ctx: &mut ClientSendCtx,
        event: &E,
        cursor: &mut Cursor<Vec<u8>>,
    ) -> bincode::Result<()> {
        self.check_type::<E>();
        let serialize: SerializeFn<E> = std::mem::transmute(self.serialize);
        (serialize)(ctx, event, cursor)
    }

    /// Deserializes an event into a cursor.
    ///
    /// # Safety
    ///
    /// The caller must ensure that this instance was created for `E`.
    unsafe fn deserialize<E: Event>(
        &self,
        ctx: &mut ServerReceiveCtx,
        cursor: &mut Cursor<&[u8]>,
    ) -> bincode::Result<E> {
        self.check_type::<E>();
        let deserialize: DeserializeFn<E> = std::mem::transmute(self.deserialize);
        (deserialize)(ctx, cursor)
    }

    fn check_type<C: Event>(&self) {
        debug_assert_eq!(
            self.type_id,
            TypeId::of::<C>(),
            "trying to call event functions with {}, but they were created with {}",
            any::type_name::<C>(),
            self.type_name,
        );
    }
}

/// Signature of client event serialization functions.
pub type SerializeFn<E> = fn(&mut ClientSendCtx, &E, &mut Cursor<Vec<u8>>) -> bincode::Result<()>;

/// Signature of client event deserialization functions.
pub type DeserializeFn<E> = fn(&mut ServerReceiveCtx, &mut Cursor<&[u8]>) -> bincode::Result<E>;

/// Signature of client event sending functions.
type SendFn = unsafe fn(&ClientEvent, &mut ClientSendCtx, &Ptr, PtrMut, &mut RepliconClient);

/// Signature of client event receiving functions.
type ReceiveFn = unsafe fn(&ClientEvent, &mut ServerReceiveCtx, PtrMut, &mut RepliconServer);

/// Signature of client event resending functions.
type ResendLocallyFn = unsafe fn(PtrMut, PtrMut);

/// Signature of client event reset functions.
type ResetFn = unsafe fn(PtrMut);

/// Typed version of [`ClientEvent::send`].
///
/// # Safety
///
/// The caller must ensure that `events` is [`Events<FromClient<E>>`], `reader` is [`ClientEventReader<E>`],
/// and `event_data` was created for `E`.
unsafe fn send<E: Event>(
    event_data: &ClientEvent,
    ctx: &mut ClientSendCtx,
    events: &Ptr,
    reader: PtrMut,
    client: &mut RepliconClient,
) {
    let reader: &mut ClientEventReader<E> = reader.deref_mut();
    for event in reader.read(events.deref()) {
        let mut cursor = Default::default();
        event_data
            .serialize(ctx, event, &mut cursor)
            .expect("client event should be serializable");

        trace!("sending event `{}`", any::type_name::<E>());
        client.send(event_data.channel_id, cursor.into_inner());
    }
}

/// Typed version of [`ClientEvent::receive`].
///
/// # Safety
///
/// The caller must ensure that `events` is [`Events<E>`]
/// and `event_data` was created for `E`.
unsafe fn receive<E: Event>(
    event_data: &ClientEvent,
    ctx: &mut ServerReceiveCtx,
    events: PtrMut,
    server: &mut RepliconServer,
) {
    let events: &mut Events<FromClient<E>> = events.deref_mut();
    for (client_id, message) in server.receive(event_data.channel_id) {
        let mut cursor = Cursor::new(&*message);
        match event_data.deserialize(ctx, &mut cursor) {
            Ok(event) => {
                trace!(
                    "applying event `{}` from `{client_id:?}`",
                    any::type_name::<E>()
                );
                events.send(FromClient { client_id, event });
            }
            Err(e) => debug!(
                "ignoring event `{}` from {client_id:?} that failed to deserialize: {e}",
                any::type_name::<E>()
            ),
        }
    }
}

/// Typed version of [`ClientEvent::resend_locally`].
///
/// # Safety
///
/// The caller must ensure that `events` is [`Events<E>`] and `server_events` is [`Events<ToClients<E>>`].
unsafe fn resend_locally<E: Event>(client_events: PtrMut, events: PtrMut) {
    let client_events: &mut Events<FromClient<E>> = client_events.deref_mut();
    let events: &mut Events<E> = events.deref_mut();
    client_events.send_batch(events.drain().map(|event| FromClient {
        client_id: ClientId::SERVER,
        event,
    }));
}

/// Typed version of [`ClientEvent::reset`].
///
/// # Safety
///
/// The caller must ensure that `events` is [`Events<E>`].
unsafe fn reset<E: Event>(events: PtrMut) {
    let events: &mut Events<E> = events.deref_mut();
    let drained_count = events.drain().count();
    if drained_count > 0 {
        warn!("discarded {drained_count} client events due to a disconnect");
    }
}

/// Tracks read events for [`ClientEventPlugin::send`].
///
/// Unlike with server events, we don't always drain all events in [`ClientEventPlugin::resend_locally`].
#[derive(Resource, Deref, DerefMut)]
struct ClientEventReader<E: Event>(ManualEventReader<E>);

impl<E: Event> FromWorld for ClientEventReader<E> {
    fn from_world(world: &mut World) -> Self {
        let events = world.resource::<Events<E>>();
        Self(events.get_reader())
    }
}

/// An event indicating that a message from client was received.
/// Emited only on server.
#[derive(Clone, Copy, Event)]
pub struct FromClient<T> {
    pub client_id: ClientId,
    pub event: T,
}

/// Default event serialization function.
pub fn default_serialize<E: Event + Serialize>(
    _ctx: &mut ClientSendCtx,
    event: &E,
    cursor: &mut Cursor<Vec<u8>>,
) -> bincode::Result<()> {
    DefaultOptions::new().serialize_into(cursor, event)
}

/// Like [`default_serialize`], but also maps entities.
pub fn default_serialize_mapped<E: Event + MapEntities + Clone + Serialize>(
    ctx: &mut ClientSendCtx,
    event: &E,
    cursor: &mut Cursor<Vec<u8>>,
) -> bincode::Result<()> {
    let mut event = event.clone();
    event.map_entities(ctx);
    DefaultOptions::new().serialize_into(cursor, &event)
}

/// Default event deserialization function.
pub fn default_deserialize<E: Event + DeserializeOwned>(
    _ctx: &mut ServerReceiveCtx,
    cursor: &mut Cursor<&[u8]>,
) -> bincode::Result<E> {
    DefaultOptions::new().deserialize_from(cursor)
}
