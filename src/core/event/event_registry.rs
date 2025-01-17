use bevy::{ecs::component::ComponentId, prelude::*};

use super::{client_event::ClientEvent, server_event::ServerEvent};

/// Registered server and client events.
#[derive(Resource, Default)]
pub(crate) struct EventRegistry {
    server: Vec<ServerEvent>,
    client: Vec<ClientEvent>,
}

impl EventRegistry {
    pub(super) fn register_server_event(&mut self, event_data: ServerEvent) {
        self.server.push(event_data);
    }

    pub(super) fn register_client_event(&mut self, event_data: ClientEvent) {
        self.client.push(event_data);
    }

    pub(super) fn make_independent(&mut self, events_id: ComponentId) {
        let event = self
            .server
            .iter_mut()
            .find(|event| event.events_id() == events_id)
            .unwrap_or_else(|| {
                panic!("event with ID {events_id:?} should be previously registered");
            });
        event.make_independent();
    }

    pub(crate) fn iter_server_events(&self) -> impl Iterator<Item = &ServerEvent> {
        self.server.iter()
    }

    pub(crate) fn iter_client_events(&self) -> impl Iterator<Item = &ClientEvent> {
        self.client.iter()
    }
}
