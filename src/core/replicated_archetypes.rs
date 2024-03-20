use bevy::{
    ecs::{
        archetype::ArchetypeId,
        component::{ComponentId, StorageType},
    },
    prelude::*,
};

use crate::core::{replication_fns::SerdeFnsId, Replication};

/// Stores cached information about all replicated archetypes.
///
/// By default it's updated with [component-based](../../index.html#component-replication) replication rules.
///
/// But it's also possible to implement custom rules:
/// - Register 'serde' and 'remove' functions inside [`ReplicationFns`](crate::core::replication_fns::ReplicationFns).
/// - Update this struct for all newly added archetypes in
/// [`ServerSet::UpdateArchetypes`](crate::server::ServerSet::UpdateArchetypes) using the registered function IDs.
/// - Update [`RemovalBuffer`](crate::server::world_buffers::RemovalBuffer) in
/// [`ServerSet::BufferRemovals`](crate::server::ServerSet::BufferRemovals) when the rule components should be removed.
#[derive(Resource)]
pub struct ReplicatedArchetypes {
    archetypes: Vec<ReplicatedArchetype>,

    /// ID of [`Replication`] component.
    marker_id: ComponentId,
}

impl ReplicatedArchetypes {
    /// Marks an archetype as being relevant for replicating entities.
    ///
    /// # Safety
    ///
    /// ID of [`ReplicatedArchetype`] should exist in [`Archetypes`](bevy::ecs::archetype::Archetypes).
    pub unsafe fn add_archetype(&mut self, replicated_archetype: ReplicatedArchetype) {
        self.archetypes.push(replicated_archetype);
    }

    /// Returns an iterator over replicated archetypes.
    pub(crate) fn iter(&self) -> impl Iterator<Item = &ReplicatedArchetype> {
        self.archetypes.iter()
    }

    /// ID of [`Replication`] component.
    #[must_use]
    pub(crate) fn marker_id(&self) -> ComponentId {
        self.marker_id
    }
}

impl FromWorld for ReplicatedArchetypes {
    fn from_world(world: &mut World) -> Self {
        Self {
            archetypes: Default::default(),
            marker_id: world.init_component::<Replication>(),
        }
    }
}

pub struct ReplicatedArchetype {
    id: ArchetypeId,
    components: Vec<ReplicatedComponent>,
}

impl ReplicatedArchetype {
    /// Creates a replicated archetype with no components.
    pub fn new(id: ArchetypeId) -> Self {
        Self {
            id,
            components: Default::default(),
        }
    }

    /// Adds a replicated component to the archetype.
    ///
    /// # Safety
    ///
    /// - Component should be present in the archetype.
    /// - Functions index and storage type should correspond to this component.
    pub unsafe fn add_component(&mut self, replicated_component: ReplicatedComponent) {
        self.components.push(replicated_component);
    }

    /// Returns the associated archetype ID.
    #[must_use]
    pub(crate) fn id(&self) -> ArchetypeId {
        self.id
    }

    /// Returns component marked as replicated.
    #[must_use]
    pub(crate) fn components(&self) -> &[ReplicatedComponent] {
        &self.components
    }
}

/// Stores information about a replicated component.
pub struct ReplicatedComponent {
    pub component_id: ComponentId,
    pub storage_type: StorageType,
    pub serde_id: SerdeFnsId,
}