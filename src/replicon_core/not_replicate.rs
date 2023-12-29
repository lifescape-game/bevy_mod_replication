use core::panic;
use std::{any, marker::PhantomData};

use bevy::{ecs::system::EntityCommands, prelude::*};

use super::replication_rules::Replication;

pub trait CommandNotReplicateExt {
    /**
    Disables replication for component `T`.

    Should only be called on the entity for which [`Replication`] was inserted at this tick.

    # Panics

    Panics if called on an entity without [`Replication`] or if it was inserted on a different tick.

    # Examples

    ```
    # use bevy::{prelude::*, ecs::system::CommandQueue};
    # use bevy_replicon::prelude::*;
    # let mut world = World::new();
    # let mut queue = CommandQueue::default();
    # let mut commands = Commands::new(&mut queue, &world);
    commands.spawn((Replication, Transform::default())).not_replicate::<Transform>();
    # queue.apply(&mut world);
    ```
    */
    fn not_replicate<T: Component>(&mut self) -> &mut Self;
}

impl CommandNotReplicateExt for EntityCommands<'_, '_, '_> {
    fn not_replicate<T: Component>(&mut self) -> &mut Self {
        self.add(|mut entity: EntityWorldMut| {
            entity.not_replicate::<T>();
        });

        self
    }
}

pub trait EntityNotReplciateExt {
    /// Same as [`CommandNotReplicateExt::not_replicate`], but for direct use on an entity.
    fn not_replicate<T: Component>(&mut self) -> &mut Self;
}

impl EntityNotReplciateExt for EntityWorldMut<'_> {
    fn not_replicate<T: Component>(&mut self) -> &mut Self {
        // SAFETY: world is not mutated and used only to obtain the tick without atomic synchronization.
        let tick = unsafe { self.world_mut().change_tick() };

        self.insert(NotReplicate::<T>(PhantomData));

        let component_name = any::type_name::<T>();
        let replication_name = any::type_name::<Replication>();
        let replication_ticks = self.get_change_ticks::<Replication>().unwrap_or_else(|| {
            panic!("disabling replication for `{component_name}` should only be done for entities with `{replication_name}`")
        });

        assert_eq!(
            tick,
            replication_ticks.added_tick(),
            "disabling replication for `{component_name}` should be done only with `{replication_name}` insertion",
        );

        self
    }
}

/// Replication will be ignored for `T` if this component is present on the same entity.
#[derive(Component, Debug)]
pub(super) struct NotReplicate<T>(PhantomData<T>);

#[cfg(test)]
mod tests {
    use bevy::ecs::system::CommandQueue;

    use super::*;

    #[test]
    #[should_panic]
    fn without_replication() {
        let mut world = World::new();

        let mut queue = CommandQueue::default();
        let mut commands = Commands::new(&mut queue, &world);
        commands.spawn_empty().not_replicate::<Transform>();
        queue.apply(&mut world);
    }

    #[test]
    #[should_panic]
    fn after_spawn() {
        let mut world = World::new();

        let mut queue = CommandQueue::default();
        let mut commands = Commands::new(&mut queue, &world);
        let entity = commands.spawn(Replication).id();
        queue.apply(&mut world);

        world.increment_change_tick();

        let mut queue = CommandQueue::default();
        let mut commands = Commands::new(&mut queue, &world);
        commands.entity(entity).not_replicate::<Transform>();
        queue.apply(&mut world);
    }
}
