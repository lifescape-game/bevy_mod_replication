use std::ops::Range;

use integer_encoding::{FixedIntWriter, VarInt, VarIntWriter};

use super::{
    component_changes::ComponentChanges, mutate_message::MutateMessage,
    serialized_data::SerializedData,
};
use crate::core::{
    channels::ReplicationChannel,
    replication::{
        change_message_flags::ChangeMessageFlags,
        replicated_clients::{client_visibility::Visibility, ReplicatedClient},
    },
    replicon_server::RepliconServer,
};

/// A message with replicated data.
///
/// Contains tick, mappings, insertions, removals, and despawns that
/// happened in this tick.
///
/// The data is serialized manually and stored in the form of ranges
/// from [`SerializedData`].
///
/// Sent over [`ReplicationChannel::Changes`] channel.
///
/// Some data is optional, and their presence is encoded in the [`ChangeMessageFlags`] bitset.
///
/// To know how much data array takes, we serialize it's length. We use `usize`,
/// but we use variable integer encoding, so they are correctly deserialized even
/// on a client with a different pointer size. However, if the server sends a value
/// larger than what a client can fit into `usize` (which is very unlikely), the client will panic.
/// This is expected, as the client can't have an array of such a size anyway.
///
/// Additionally, we don't serialize the size for the last array and
/// on deserialization just consume all remaining bytes.
///
/// Stored inside [`ReplicationMessages`](super::ReplicationMessages).
#[derive(Default)]
pub(crate) struct ChangeMessage {
    /// Mappings for client's pre-spawned entities.
    ///
    /// Serialized as single continuous chunk of entity pairs.
    ///
    /// Mappings should be processed first, so all referenced entities after it will behave correctly.
    ///
    /// See also [`ClientEntityMap`](crate::server::client_entity_map::ClientEntityMap).
    mappings: Range<usize>,

    /// Number of pairs encoded in [`Self::mappings`].
    mappings_len: usize,

    /// Despawns that happened in this tick.
    ///
    /// Since clients may see different entities, it's serialized as multiple chunks of entities.
    /// I.e. serialized server despawns may have holes for clients due to visibility differences.
    despawns: Vec<Range<usize>>,

    /// Number of depspawned entities.
    ///
    /// May not be equal to the length of [`Self::despawns`] since adjacent ranges are merged together.
    despawns_len: usize,

    /// Component removals that happened in this tick.
    ///
    /// Serialized as a list of pairs of entity chunk and a list of
    /// [`FnsId`](crate::core::replication::replication_registry::FnsId)
    /// serialized as a single chunk.
    removals: Vec<ComponentRemovals>,

    /// Component insertions or mutations that happened in this tick.
    ///
    /// Serialized as a list of pairs of entity chunk and multiple chunks with changed components.
    /// Components are stored in multiple chunks because newly connected clients may need to serialize all components,
    /// while previously connected clients only need the components spawned during this tick.
    ///
    /// Usually mutations are stored in [`MutateMessage`], but if an entity has any insertions or removal,
    /// or the entity just became visible for a client, we serialize it as part of the change message to keep entity updates atomic.
    changes: Vec<ComponentChanges>,

    /// Visibility of the entity for which component changes are being written.
    ///
    /// Updated after [`Self::start_entity_changes`].
    entity_visibility: Visibility,

    /// Indicates that an entity has been written since the
    /// last call of [`Self::start_entity_changes`].
    entity_written: bool,

    /// Intermediate buffer to reuse allocated memory from [`Self::changes`].
    buffer: Vec<Vec<Range<usize>>>,
}

impl ChangeMessage {
    pub(crate) fn set_mappings(&mut self, mappings: Range<usize>, len: usize) {
        self.mappings = mappings;
        self.mappings_len = len;
    }

    pub(crate) fn add_despawn(&mut self, entity: Range<usize>) {
        self.despawns_len += 1;
        if let Some(last) = self.despawns.last_mut() {
            // Append to previous range if possible.
            if last.end == entity.start {
                last.end = entity.end;
                return;
            }
        }
        self.despawns.push(entity);
    }

    pub(crate) fn add_removals(
        &mut self,
        entity: Range<usize>,
        ids_len: usize,
        fn_ids: Range<usize>,
    ) {
        self.removals.push(ComponentRemovals {
            entity,
            ids_len,
            fn_ids,
        });
    }

    /// Updates internal state to start writing changed components for an entity with the given visibility.
    ///
    /// Entities and their data are written lazily during the iteration.
    /// See [`Self::add_changed_entity`] and [`Self::add_changed_component`].
    pub(crate) fn start_entity_changes(&mut self, visibility: Visibility) {
        self.entity_visibility = visibility;
        self.entity_written = false;
    }

    /// Visibility from the last call of [`Self::start_entity_changes`].
    pub(crate) fn entity_visibility(&self) -> Visibility {
        self.entity_visibility
    }

    /// Returns `true` if [`Self::add_changed_entity`] were called since the last
    /// call of [`Self::start_entity_changes`].
    pub(crate) fn entity_written(&mut self) -> bool {
        self.entity_written
    }

    /// Adds an entity chunk.
    pub(crate) fn add_changed_entity(&mut self, entity: Range<usize>) {
        let components = self.buffer.pop().unwrap_or_default();
        self.changes.push(ComponentChanges {
            entity,
            components_len: 0,
            components,
        });
        self.entity_written = true;
    }

    /// Adds a component chunk to the last added entity from [`Self::add_changed_entity`].
    pub(crate) fn add_inserted_component(&mut self, component: Range<usize>) {
        let changes = self
            .changes
            .last_mut()
            .expect("entity should be written before adding components");

        changes.add_component(component);
    }

    /// Takes last mutated entity with its component chunks from the mutate message.
    pub(crate) fn take_mutations(&mut self, mutate_message: &mut MutateMessage) {
        if !mutate_message.mutations_written() {
            return;
        }

        let mutations = mutate_message
            .last_mutations()
            .expect("entity should be written");

        if !self.entity_written {
            let components = self.buffer.pop().unwrap_or_default();
            let changes = ComponentChanges {
                entity: mutations.entity.clone(),
                components_len: 0,
                components,
            };
            self.changes.push(changes);
        }
        let changes = self.changes.last_mut().unwrap();
        debug_assert_eq!(mutations.entity, changes.entity);
        changes.extend(mutations);

        mutate_message.pop_mutations();
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.changes.is_empty()
            && self.despawns.is_empty()
            && self.removals.is_empty()
            && self.mappings.is_empty()
    }

    pub(crate) fn send(
        &self,
        server: &mut RepliconServer,
        client: &ReplicatedClient,
        serialized: &SerializedData,
        server_tick: Range<usize>,
    ) -> bincode::Result<()> {
        let flags = self.flags();
        let last_flag = flags.last();

        // Precalculate size first to avoid extra allocations.
        let mut message_size = size_of::<ChangeMessageFlags>() + server_tick.len();
        for (_, flag) in flags.iter_names() {
            match flag {
                ChangeMessageFlags::MAPPINGS => {
                    if flag != last_flag {
                        message_size += self.mappings_len.required_space();
                    }
                    message_size += self.mappings.len();
                }
                ChangeMessageFlags::DESPAWNS => {
                    if flag != last_flag {
                        message_size += self.despawns_len.required_space();
                    }
                    message_size += self.despawns.iter().map(|range| range.len()).sum::<usize>();
                }
                ChangeMessageFlags::REMOVALS => {
                    if flag != last_flag {
                        message_size += self.removals.len().required_space();
                    }
                    message_size += self
                        .removals
                        .iter()
                        .map(|removals| removals.size())
                        .sum::<usize>();
                }
                ChangeMessageFlags::CHANGES => {
                    debug_assert_eq!(flag, last_flag);
                    message_size += self
                        .changes
                        .iter()
                        .map(|changes| {
                            changes.entity.len()
                                + changes.components_len.required_space()
                                + changes.components_size()
                        })
                        .sum::<usize>();
                }
                _ => unreachable!("iteration should yield only named flags"),
            }
        }

        let mut message = Vec::with_capacity(message_size);
        message.write_fixedint(flags.bits())?;
        message.extend_from_slice(&serialized[server_tick]);
        for (_, flag) in flags.iter_names() {
            match flag {
                ChangeMessageFlags::MAPPINGS => {
                    // Always write size since the message can't have only mappings.
                    // Otherwise this would mean that the client already received the mapped
                    // entity and it's already mapped or server sends an invisible entity which
                    // is an error.
                    debug_assert_ne!(flag, last_flag);
                    message.write_varint(self.mappings_len)?;
                    message.extend_from_slice(&serialized[self.mappings.clone()]);
                }
                ChangeMessageFlags::DESPAWNS => {
                    if flag != last_flag {
                        message.write_varint(self.despawns_len)?;
                    }
                    for range in &self.despawns {
                        message.extend_from_slice(&serialized[range.clone()]);
                    }
                }
                ChangeMessageFlags::REMOVALS => {
                    if flag != last_flag {
                        message.write_varint(self.removals.len())?;
                    }
                    for removals in &self.removals {
                        message.extend_from_slice(&serialized[removals.entity.clone()]);
                        message.write_varint(removals.ids_len)?;
                        message.extend_from_slice(&serialized[removals.fn_ids.clone()]);
                    }
                }
                ChangeMessageFlags::CHANGES => {
                    // Changes are always last, don't write len for it.
                    for changes in &self.changes {
                        message.extend_from_slice(&serialized[changes.entity.clone()]);
                        message.write_varint(changes.components_len)?;
                        for component in &changes.components {
                            message.extend_from_slice(&serialized[component.clone()]);
                        }
                    }
                }
                _ => unreachable!("iteration should yield only named flags"),
            }
        }

        debug_assert_eq!(message.len(), message_size);

        server.send(client.id(), ReplicationChannel::Changes, message);

        Ok(())
    }

    fn flags(&self) -> ChangeMessageFlags {
        let mut flags = ChangeMessageFlags::default();

        if !self.mappings.is_empty() {
            flags |= ChangeMessageFlags::MAPPINGS;
        }
        if !self.despawns.is_empty() {
            flags |= ChangeMessageFlags::DESPAWNS;
        }
        if !self.removals.is_empty() {
            flags |= ChangeMessageFlags::REMOVALS;
        }
        if !self.changes.is_empty() {
            flags |= ChangeMessageFlags::CHANGES;
        }

        flags
    }

    /// Clears all chunks.
    ///
    /// Keeps allocated memory for reuse.
    pub(super) fn clear(&mut self) {
        self.mappings = Default::default();
        self.mappings_len = 0;
        self.despawns.clear();
        self.despawns_len = 0;
        self.removals.clear();
        self.buffer
            .extend(self.changes.drain(..).map(|mut changes| {
                changes.components.clear();
                changes.components
            }));
    }
}

struct ComponentRemovals {
    entity: Range<usize>,
    ids_len: usize,
    fn_ids: Range<usize>,
}

impl ComponentRemovals {
    fn size(&self) -> usize {
        self.entity.len() + self.ids_len.required_space() + self.fn_ids.len()
    }
}
