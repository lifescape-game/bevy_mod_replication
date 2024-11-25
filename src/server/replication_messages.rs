pub(super) mod change_message;
mod component_changes;
pub(super) mod mutate_message;
pub(super) mod serialized_data;

use change_message::ChangeMessage;
use mutate_message::MutateMessage;

/// Accumulates replication messages.
///
/// Messages are serialized manually into [`SerializedData`](serialized_data::SerializedData)
/// and store only ranges that point to data. This helps reduce allocations and share
/// serialized data across messages.
#[derive(Default)]
pub(crate) struct ReplicationMessages {
    messages: Vec<(ChangeMessage, MutateMessage)>,
    len: usize,
}

impl ReplicationMessages {
    /// Initializes messages for each client.
    ///
    /// Reuses already allocated messages.
    /// Creates new messages if the number of clients is bigger then the number of allocated messages.
    /// If there are more messages than the number of clients, then the extra messages remain untouched
    /// and [`Self::iter_mut`] will not include them.
    pub(super) fn reset(&mut self, clients_count: usize) {
        self.len = clients_count;

        let additional = clients_count.saturating_sub(self.messages.len());
        self.messages.reserve(additional);

        for index in 0..clients_count {
            if let Some((change_message, mutate_message)) = self.messages.get_mut(index) {
                change_message.clear();
                mutate_message.clear();
            } else {
                self.messages.push(Default::default());
            }
        }
    }

    /// Returns iterator over messages for each client.
    pub(super) fn iter_mut(&mut self) -> impl Iterator<Item = &mut (ChangeMessage, MutateMessage)> {
        self.messages.iter_mut().take(self.len)
    }
}
