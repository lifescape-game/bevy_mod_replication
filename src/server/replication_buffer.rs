use std::{io::Cursor, mem};

use bevy::{ecs::component::Tick, prelude::*, ptr::Ptr};
use bevy_renet::renet::{Bytes, ClientId, RenetServer};
use bincode::{DefaultOptions, Options};
use varint_rs::VarintWriter;

use super::ClientMapping;
use crate::replicon_core::{
    replication_rules::{ReplicationId, ReplicationInfo},
    replicon_tick::RepliconTick,
    REPLICATION_CHANNEL_ID,
};

/// A reusable buffer with replicated data for a client.
///
/// See also [Limits](../index.html#limits)
pub(crate) struct ReplicationBuffer {
    /// ID of a client for which this buffer is written.
    client_id: ClientId,

    /// Last system tick acknowledged by the client.
    ///
    /// Used for changes preparation.
    system_tick: Tick,

    /// Send buffer data even if it doesn't contain replication data.
    ///
    /// See also [`Self::send_to`]
    send_empty: bool,

    /// Serialized data.
    cursor: Cursor<Vec<u8>>,

    /// Position of the array from last call of [`Self::start_array`].
    array_pos: u64,

    /// Length of the array that updated automatically after writing data.
    array_len: u16,

    /// The number of arrays excluding trailing empty arrays.
    arrays_with_data: usize,

    /// The number of empty arrays at the end. Can be removed using [`Self::trim_empty_arrays`]
    trailing_empty_arrays: usize,

    /// Position of entity after [`Self::start_entity_data`] or its data after [`Self::write_data_entity`].
    entity_data_pos: u64,

    /// Length of the data for entity that updated automatically after writing data.
    entity_data_len: u8,

    /// Entity from last call of [`Self::start_entity_data`].
    data_entity: Entity,
}

impl ReplicationBuffer {
    /// Creates a new buffer with assigned client ID.
    ///
    /// `replicon_tick` is the current tick that will be written into
    ///  the buffer to read by client on receive.
    ///
    /// `system_tick` is the last acknowledged system tick for this client.
    ///  Changes since this tick should be written into the buffer.
    ///
    /// If `send_empty` is set to `true`, then [`Self::send_to`]
    /// will send the buffer data even if it contains only replicon tick.
    pub(super) fn new(
        replicon_tick: RepliconTick,
        client_id: ClientId,
        system_tick: Tick,
        send_empty: bool,
    ) -> bincode::Result<Self> {
        let mut cursor = Default::default();
        bincode::serialize_into(&mut cursor, &replicon_tick)?;

        Ok(Self {
            client_id,
            system_tick,
            send_empty,
            cursor,
            array_pos: Default::default(),
            array_len: Default::default(),
            arrays_with_data: Default::default(),
            trailing_empty_arrays: Default::default(),
            entity_data_pos: Default::default(),
            entity_data_len: Default::default(),
            data_entity: Entity::PLACEHOLDER,
        })
    }

    /// Clears the buffer and assigns it to a different client ID.
    ///
    /// Keeps allocated capacity of the buffer.
    pub(super) fn reset(
        &mut self,
        replicon_tick: RepliconTick,
        client_id: ClientId,
        system_tick: Tick,
        send_empty: bool,
    ) -> bincode::Result<()> {
        self.client_id = client_id;
        self.system_tick = system_tick;
        self.send_empty = send_empty;
        self.cursor.set_position(0);
        self.cursor.get_mut().clear();
        self.arrays_with_data = 0;
        self.trailing_empty_arrays = 0;
        bincode::serialize_into(&mut self.cursor, &replicon_tick)?;

        Ok(())
    }

    /// Returns the designated client ID.
    pub(super) fn client_id(&self) -> ClientId {
        self.client_id
    }

    /// Returns the last acknowledged system tick for the designated client.
    pub(super) fn system_tick(&self) -> Tick {
        self.system_tick
    }

    /// Starts writing array by remembering its position to write length after.
    ///
    /// Arrays can contain entity data or despawns inside.
    /// Length will be increased automatically after writing data.
    /// See also [`Self::end_array`], [`Self::start_entity_data`] and [`Self::write_despawn`].
    pub(super) fn start_array(&mut self) {
        debug_assert_eq!(self.array_len, 0);

        self.array_pos = self.cursor.position();
        self.cursor
            .set_position(self.array_pos + mem::size_of_val(&self.array_len) as u64);
    }

    /// Ends writing array by writing its length into the last remembered position.
    ///
    /// See also [`Self::start_array`].
    pub(super) fn end_array(&mut self) -> bincode::Result<()> {
        if self.array_len != 0 {
            let previous_pos = self.cursor.position();
            self.cursor.set_position(self.array_pos);

            bincode::serialize_into(&mut self.cursor, &self.array_len)?;

            self.cursor.set_position(previous_pos);
            self.array_len = 0;
            self.arrays_with_data += 1;
            self.trailing_empty_arrays = 0;
        } else {
            self.trailing_empty_arrays += 1;
            self.cursor.set_position(self.array_pos);
            bincode::serialize_into(&mut self.cursor, &self.array_len)?;
        }

        Ok(())
    }

    /// Serializes entity to entity mapping as an array element.
    ///
    /// Should be called only inside array.
    /// Increases array length by 1.
    /// See also [`Self::start_array`].
    pub(super) fn write_client_mapping(&mut self, mapping: &ClientMapping) -> bincode::Result<()> {
        serialize_entity(&mut self.cursor, mapping.server_entity)?;
        serialize_entity(&mut self.cursor, mapping.client_entity)?;
        self.array_len = self
            .array_len
            .checked_add(1)
            .ok_or(bincode::ErrorKind::SizeLimit)?;

        Ok(())
    }

    /// Serializes `entity` as an array element.
    ///
    /// Should be called only inside array.
    /// Increases array length by 1.
    /// See also [`Self::start_array`].
    pub(super) fn write_entity(&mut self, entity: Entity) -> bincode::Result<()> {
        serialize_entity(&mut self.cursor, entity)?;
        self.array_len = self
            .array_len
            .checked_add(1)
            .ok_or(bincode::ErrorKind::SizeLimit)?;

        Ok(())
    }

    /// Crops empty arrays at the end.
    ///
    /// Should only be called after all arrays have been written, because
    /// removed array somewhere the middle cannot be detected during deserialization.
    fn trim_empty_arrays(&mut self) {
        let used_len = self.cursor.get_ref().len()
            - self.trailing_empty_arrays * mem::size_of_val(&self.array_len);
        self.cursor.get_mut().truncate(used_len);
    }

    /// Starts writing entity and its data by remembering `entity`.
    ///
    /// Arrays can contain component changes or removals inside.
    /// Length will be increased automatically after writing data.
    /// Entity will be written lazily after first data write and its position will be remembered to write length later.
    /// See also [`Self::end_entity_data`], [`Self::write_current_entity`], [`Self::write_change`]
    /// and [`Self::write_removal`].
    pub(super) fn start_entity_data(&mut self, entity: Entity) {
        debug_assert_eq!(self.entity_data_len, 0);

        self.data_entity = entity;
        self.entity_data_pos = self.cursor.position();
    }

    /// Writes entity for current data and updates remembered position for it to write length later.
    ///
    /// Should be called only after first data write.
    fn write_data_entity(&mut self) -> bincode::Result<()> {
        serialize_entity(&mut self.cursor, self.data_entity)?;
        self.entity_data_pos = self.cursor.position();
        self.cursor
            .set_position(self.entity_data_pos + mem::size_of_val(&self.entity_data_len) as u64);

        Ok(())
    }

    /// Ends writing entity data by writing its length into the last remembered position.
    ///
    /// If the entity data is empty, nothing will be written.
    /// See also [`Self::start_array`], [`Self::write_current_entity`], [`Self::write_change`] and
    /// [`Self::write_removal`].
    pub(super) fn end_entity_data(&mut self) -> bincode::Result<()> {
        if self.entity_data_len != 0 {
            let previous_pos = self.cursor.position();
            self.cursor.set_position(self.entity_data_pos);

            bincode::serialize_into(&mut self.cursor, &self.entity_data_len)?;

            self.cursor.set_position(previous_pos);
            self.entity_data_len = 0;
            self.array_len = self
                .array_len
                .checked_add(1)
                .ok_or(bincode::ErrorKind::SizeLimit)?;
        } else {
            self.cursor.set_position(self.entity_data_pos);
        }

        Ok(())
    }

    /// Serializes `replication_id` and its component from `ptr` as an element of entity data.
    ///
    /// Should be called only inside entity data.
    /// Increases entity data length by 1.
    /// See also [`Self::start_entity_data`].
    pub(super) fn write_component(
        &mut self,
        replication_info: &ReplicationInfo,
        replication_id: ReplicationId,
        ptr: Ptr,
    ) -> bincode::Result<()> {
        if self.entity_data_len == 0 {
            self.write_data_entity()?;
        }

        DefaultOptions::new().serialize_into(&mut self.cursor, &replication_id)?;
        (replication_info.serialize)(ptr, &mut self.cursor)?;
        self.entity_data_len += 1;

        Ok(())
    }

    /// Serializes `replication_id` as an element of entity data.
    ///
    /// Should be called only inside entity data.
    /// Increases entity data length by 1.
    /// See also [`Self::start_entity_data`].
    pub(super) fn write_replication_id(
        &mut self,
        replication_id: ReplicationId,
    ) -> bincode::Result<()> {
        if self.entity_data_len == 0 {
            self.write_data_entity()?;
        }

        DefaultOptions::new().serialize_into(&mut self.cursor, &replication_id)?;
        self.entity_data_len += 1;

        Ok(())
    }

    /// Sends the buffer data to the designated client.
    ///
    /// [`Self::reset`] should be called after it to use this buffer again.
    pub(super) fn send_to(&mut self, server: &mut RenetServer) {
        debug_assert_eq!(self.array_len, 0);
        debug_assert_eq!(self.entity_data_len, 0);

        if self.arrays_with_data > 0 || self.send_empty {
            self.trim_empty_arrays();

            trace!("sending replication message to client {}", self.client_id);
            server.send_message(
                self.client_id,
                REPLICATION_CHANNEL_ID,
                Bytes::copy_from_slice(self.cursor.get_ref()),
            );
        } else {
            trace!("no changes to send for client {}", self.client_id);
        }
    }
}

/// Serializes `entity` by writing its index and generation as separate varints.
///
/// The index is first prepended with a bit flag to indicate if the generation
/// is serialized or not (it is not serialized if equal to zero).
fn serialize_entity(cursor: &mut Cursor<Vec<u8>>, entity: Entity) -> bincode::Result<()> {
    let mut flagged_index = (entity.index() as u64) << 1;
    let flag = entity.generation() > 0;
    flagged_index |= flag as u64;

    cursor.write_u64_varint(flagged_index)?;
    if flag {
        cursor.write_u32_varint(entity.generation())?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trimming_arrays() -> bincode::Result<()> {
        let mut buffer =
            ReplicationBuffer::new(RepliconTick(0), ClientId::from_raw(0), Tick::new(0), false)?;

        let begin_len = buffer.cursor.get_ref().len();
        for _ in 0..3 {
            buffer.start_array();
            buffer.end_array()?;
        }

        buffer.trim_empty_arrays();

        assert_eq!(buffer.cursor.get_ref().len(), begin_len);

        Ok(())
    }
}
