use core::mem;

use bevy::{
    ecs::{
        component::ComponentId,
        entity::{hash_map::EntityHashMap, hash_set::EntityHashSet},
    },
    prelude::*,
};
use log::trace;

use crate::{
    server::replicated_archetypes::ReplicatedArchetype,
    shared::replication::{
        registry::{ComponentIndex, ReplicationRegistry},
        rules::component::ComponentRule,
    },
};

/// A buffered removal of one replicated component.
pub(super) type Removal = (ComponentIndex, ComponentRule);

/// Buffer with removed components for the current tick, and with removals
/// withheld from earlier ticks until the confirmed state shows them.
#[derive(Resource, Deref, Default)]
pub(super) struct RemovalBuffer {
    /// Component removals of the current tick grouped by entity.
    #[deref]
    removals: EntityHashMap<Vec<Removal>>,

    /// Removals from earlier ticks that the confirmed state at the last
    /// [`ServerTick`](super::server_tick::ServerTick) did not show yet.
    withheld: EntityHashMap<Vec<Removal>>,

    /// An emptied map swapped in for [`Self::withheld`] by
    /// [`Self::take_pending`], stored to reuse allocated capacity.
    spare: EntityHashMap<Vec<Removal>>,

    /// Entities with removals released for sending this tick.
    released: EntityHashSet,

    /// [`Vec`]s from removals.
    ///
    /// All data is cleared before the insertion.
    /// Stored to reuse allocated capacity.
    pool: Vec<Vec<Removal>>,
}

impl RemovalBuffer {
    pub(super) fn insert(
        &mut self,
        entity: Entity,
        components: &[ComponentId],
        archetype: &ReplicatedArchetype,
        registry: &ReplicationRegistry,
    ) {
        let entity_removals = self
            .removals
            .entry(entity)
            .or_insert_with(|| self.pool.pop().unwrap_or_default());

        for &id in components {
            let Some(&rule) = archetype.find_rule(id) else {
                trace!("skipping non-replicated `{id:?}` removal for `{entity}`");
                continue;
            };

            if entity_removals
                .iter()
                .any(|(_, buffered)| buffered.id == rule.id)
            {
                continue;
            }
            let (component_index, ..) = registry.get(rule.fns_id);
            trace!("buffering `{:?}` removal for `{entity}`", rule.fns_id);
            entity_removals.push((component_index, rule));
        }
    }

    /// Takes the removals of the current tick together with those withheld
    /// from earlier ticks, each removal once.
    ///
    /// Hand the drained map back with [`Self::return_pending`] to keep its
    /// allocation.
    pub(super) fn take_pending(&mut self) -> EntityHashMap<Vec<Removal>> {
        for (entity, mut components) in self.removals.drain() {
            match self.withheld.get_mut(&entity) {
                Some(withheld) => {
                    for &removal in &components {
                        if !withheld.iter().any(|(_, rule)| rule.id == removal.1.id) {
                            withheld.push(removal);
                        }
                    }
                    components.clear();
                    self.pool.push(components);
                }
                None => {
                    self.withheld.insert(entity, components);
                }
            }
        }
        mem::replace(&mut self.withheld, mem::take(&mut self.spare))
    }

    /// Stores the map drained after [`Self::take_pending`] for reuse.
    pub(super) fn return_pending(&mut self, pending: EntityHashMap<Vec<Removal>>) {
        self.spare = pending;
    }

    /// Keeps `components` for the next tick's [`Self::take_pending`].
    pub(super) fn withhold(&mut self, entity: Entity, components: Vec<Removal>) {
        self.withheld.insert(entity, components);
    }

    /// Removals of `entity` withheld for a later tick.
    pub(super) fn withheld_of(&self, entity: Entity) -> &[Removal] {
        self.withheld.get(&entity).map_or(&[], Vec::as_slice)
    }

    /// Drops every removal of `entity`, which no longer replicates.
    pub(super) fn discard(&mut self, entity: Entity) {
        let removals = [self.removals.remove(&entity), self.withheld.remove(&entity)];
        for components in removals.into_iter().flatten() {
            self.recycle(components);
        }
    }

    /// Returns an emptied [`Vec`] for reuse.
    pub(super) fn recycle(&mut self, mut components: Vec<Removal>) {
        components.clear();
        self.pool.push(components);
    }

    /// Returns an empty [`Vec`] for removals, reusing allocated capacity.
    pub(super) fn take_vec(&mut self) -> Vec<Removal> {
        self.pool.pop().unwrap_or_default()
    }

    pub(super) fn mark_released(&mut self, entity: Entity) {
        self.released.insert(entity);
    }

    pub(super) fn is_released(&self, entity: Entity) -> bool {
        self.released.contains(&entity)
    }

    /// Clears the current tick's removals; withheld ones stay.
    ///
    /// Keeps the allocated memory for reuse.
    pub(super) fn clear(&mut self) {
        self.pool
            .extend(self.removals.drain().map(|(_, mut components)| {
                components.clear();
                components
            }));
        self.released.clear();
    }

    /// Clears all removals, withheld ones included.
    pub(super) fn reset(&mut self) {
        self.clear();
        self.pool
            .extend(self.withheld.drain().map(|(_, mut components)| {
                components.clear();
                components
            }));
    }
}

#[cfg(test)]
mod tests {
    use bevy::{
        ecs::world::unsafe_world_cell::UnsafeWorldCell, ptr::Ptr, state::app::StatesPlugin,
    };
    use serde::{Deserialize, Serialize};

    use super::*;
    use crate::{
        prelude::*,
        shared::{replication::rules::ReplicationRules, replicon_tick::RepliconTick},
    };

    #[test]
    fn not_replicated() {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, StatesPlugin, RepliconPlugins))
            .finish();

        app.world_mut()
            .resource_mut::<NextState<ServerState>>()
            .set(ServerState::Running);
        app.update();

        app.world_mut().spawn((Replicated, A)).remove::<A>();

        let removal_buffer = app.world().resource::<RemovalBuffer>();
        assert!(removal_buffer.is_empty());
    }

    #[test]
    fn component() {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, StatesPlugin, RepliconPlugins))
            .replicate::<A>()
            .finish();

        app.world_mut()
            .resource_mut::<NextState<ServerState>>()
            .set(ServerState::Running);
        app.update();

        let entity = app.world_mut().spawn((Replicated, A)).remove::<A>().id();

        let removal_buffer = app.world().resource::<RemovalBuffer>();
        assert_eq!(removal_buffer.len(), 1);

        let removal_ids = removal_buffer.get(&entity).unwrap();
        assert_eq!(removal_ids.len(), 1);
    }

    #[test]
    fn bundle() {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, StatesPlugin, RepliconPlugins))
            .replicate_bundle::<(A, B)>()
            .finish();

        app.world_mut()
            .resource_mut::<NextState<ServerState>>()
            .set(ServerState::Running);
        app.update();

        let entity = app
            .world_mut()
            .spawn((Replicated, A, B))
            .remove::<(A, B)>()
            .id();

        let removal_buffer = app.world().resource::<RemovalBuffer>();
        assert_eq!(removal_buffer.len(), 1);

        let removal_ids = removal_buffer.get(&entity).unwrap();
        assert_eq!(removal_ids.len(), 2);
    }

    #[test]
    fn part_of_bundle() {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, StatesPlugin, RepliconPlugins))
            .replicate_bundle::<(A, B)>()
            .finish();

        app.world_mut()
            .resource_mut::<NextState<ServerState>>()
            .set(ServerState::Running);
        app.update();

        let entity = app.world_mut().spawn((Replicated, A, B)).remove::<A>().id();

        let removal_buffer = app.world().resource::<RemovalBuffer>();
        assert_eq!(removal_buffer.len(), 1);

        let removal_ids = removal_buffer.get(&entity).unwrap();
        assert_eq!(removal_ids.len(), 1);
    }

    #[test]
    fn bundle_with_subset() {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, StatesPlugin, RepliconPlugins))
            .replicate::<A>()
            .replicate_bundle::<(A, B)>()
            .finish();

        app.world_mut()
            .resource_mut::<NextState<ServerState>>()
            .set(ServerState::Running);
        app.update();

        let entity = app
            .world_mut()
            .spawn((Replicated, A, B))
            .remove::<(A, B)>()
            .id();

        let removal_buffer = app.world().resource::<RemovalBuffer>();
        assert_eq!(removal_buffer.len(), 1);

        let removal_ids = removal_buffer.get(&entity).unwrap();
        assert_eq!(removal_ids.len(), 2);
    }

    #[test]
    fn part_of_bundle_with_subset() {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, StatesPlugin, RepliconPlugins))
            .replicate::<A>()
            .replicate_bundle::<(A, B)>()
            .finish();

        app.world_mut()
            .resource_mut::<NextState<ServerState>>()
            .set(ServerState::Running);
        app.update();

        let entity = app.world_mut().spawn((Replicated, A, B)).remove::<A>().id();

        let removal_buffer = app.world().resource::<RemovalBuffer>();
        assert_eq!(removal_buffer.len(), 1);

        let removal_ids = removal_buffer.get(&entity).unwrap();
        let [(_, rule)] = removal_ids.as_slice().try_into().unwrap();

        let rules = app.world().resource::<ReplicationRules>();
        let bundle_rule = rules.iter().find(|r| r.components.len() == 2).unwrap();
        assert!(
            bundle_rule
                .components
                .iter()
                .any(|r| r.fns_id == rule.fns_id),
            "removal should be long to the bundle"
        );
    }

    #[test]
    fn despawn() {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, StatesPlugin, RepliconPlugins))
            .replicate::<A>()
            .finish();

        app.world_mut()
            .resource_mut::<NextState<ServerState>>()
            .set(ServerState::Running);
        app.update();

        app.world_mut().spawn((Replicated, A)).despawn();

        let removal_buffer = app.world().resource::<RemovalBuffer>();
        assert!(
            removal_buffer.is_empty(),
            "despawns shouldn't be counted as removals"
        );
    }

    #[test]
    fn removed_twice_in_a_tick() {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, StatesPlugin, RepliconPlugins))
            .replicate::<A>()
            .finish();

        app.world_mut()
            .resource_mut::<NextState<ServerState>>()
            .set(ServerState::Running);
        app.update();

        let entity = app
            .world_mut()
            .spawn((Replicated, A))
            .remove::<A>()
            .insert(A)
            .remove::<A>()
            .id();

        let removal_buffer = app.world().resource::<RemovalBuffer>();
        assert_eq!(removal_buffer.get(&entity).unwrap().len(), 1);
    }

    #[test]
    fn removed_twice_under_different_rules() {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, StatesPlugin, RepliconPlugins))
            .replicate::<A>()
            .replicate_bundle::<(A, B)>()
            .finish();

        app.world_mut()
            .resource_mut::<NextState<ServerState>>()
            .set(ServerState::Running);
        app.update();

        let entity = app
            .world_mut()
            .spawn((Replicated, A, B))
            .remove::<A>()
            .insert(A)
            .remove::<B>()
            .remove::<A>()
            .id();

        let removal_buffer = app.world().resource::<RemovalBuffer>();
        assert_eq!(removal_buffer.get(&entity).unwrap().len(), 2);
    }

    #[test]
    fn stays_withheld_while_removed_later() {
        let (mut app, entity) = withheld_app();

        app.update();

        let removal_buffer = app.world().resource::<RemovalBuffer>();
        assert_eq!(removal_buffer.withheld_of(entity).len(), 1);
    }

    #[test]
    fn undone_withheld_removal_is_dropped() {
        let (mut app, entity) = withheld_app();
        let mut entity_mut = app.world_mut().entity_mut(entity);
        entity_mut.insert(A);
        entity_mut.get_mut::<ConfirmedA>().unwrap().removed_after = false;

        app.update();

        let removal_buffer = app.world().resource::<RemovalBuffer>();
        assert!(removal_buffer.withheld_of(entity).is_empty());
    }

    #[test]
    fn removal_absent_live_stays_withheld_until_the_history_records_it() {
        let (mut app, entity) = withheld_app();
        app.world_mut()
            .get_mut::<ConfirmedA>(entity)
            .unwrap()
            .removed_after = false;

        app.update();

        let removal_buffer = app.world().resource::<RemovalBuffer>();
        assert_eq!(removal_buffer.withheld_of(entity).len(), 1);
    }

    #[test]
    fn unreplicated_entity_drops_withheld_removal() {
        let (mut app, entity) = withheld_app();
        app.world_mut().entity_mut(entity).remove::<Replicated>();

        app.update();

        let removal_buffer = app.world().resource::<RemovalBuffer>();
        assert!(removal_buffer.withheld_of(entity).is_empty());
    }

    #[test]
    fn released_removal_keeps_the_withheld_allocation() {
        let (mut app, entity) = withheld_app();
        let mut confirmed = app.world_mut().get_mut::<ConfirmedA>(entity).unwrap();
        confirmed.removed = true;
        confirmed.removed_after = false;

        app.update();

        let removal_buffer = app.world().resource::<RemovalBuffer>();
        assert!(removal_buffer.withheld_of(entity).is_empty());
        assert_ne!(removal_buffer.withheld.capacity(), 0);
    }

    /// An app with a removal of [`A`] withheld, whose [`ConfirmedA`] decides the
    /// next send tick's confirmed state.
    fn withheld_app() -> (App, Entity) {
        let mut app = App::new();
        app.add_plugins((
            MinimalPlugins,
            StatesPlugin,
            RepliconPlugins.set(ServerPlugin::new(PostUpdate)),
        ))
        .replicate::<A>()
        .finish();
        app.world_mut()
            .resource_mut::<ConfirmedReplicationSource>()
            .set(confirmed_a, confirmed_a_removed_after);
        app.world_mut()
            .resource_mut::<NextState<ServerState>>()
            .set(ServerState::Running);
        app.update();

        let entity = app
            .world_mut()
            .spawn((
                Replicated,
                A,
                ConfirmedA {
                    value: A,
                    removed: false,
                    removed_after: true,
                },
            ))
            .remove::<A>()
            .id();
        app.update();
        assert_eq!(
            app.world()
                .resource::<RemovalBuffer>()
                .withheld_of(entity)
                .len(),
            1
        );

        (app, entity)
    }

    #[derive(Component)]
    struct ConfirmedA {
        value: A,
        removed: bool,
        removed_after: bool,
    }

    unsafe fn confirmed_a<'w>(
        world: UnsafeWorldCell<'w>,
        entity: Entity,
        _component: ComponentId,
        _tick: RepliconTick,
    ) -> ConfirmedLookup<'w> {
        match confirmed_of(world, entity) {
            Some(confirmed) if confirmed.removed => ConfirmedLookup::Removed,
            Some(confirmed) => ConfirmedLookup::Confirmed(Ptr::from(&confirmed.value)),
            None => ConfirmedLookup::Live,
        }
    }

    unsafe fn confirmed_a_removed_after(
        world: UnsafeWorldCell<'_>,
        entity: Entity,
        _component: ComponentId,
        _tick: RepliconTick,
    ) -> bool {
        confirmed_of(world, entity).is_some_and(|confirmed| confirmed.removed_after)
    }

    fn confirmed_of(world: UnsafeWorldCell<'_>, entity: Entity) -> Option<&ConfirmedA> {
        // SAFETY: the send systems only read `ConfirmedA`.
        world
            .get_entity(entity)
            .ok()
            .and_then(|entity| unsafe { entity.get::<ConfirmedA>() })
    }

    #[derive(Component, Serialize, Deserialize)]
    struct A;

    #[derive(Component, Serialize, Deserialize)]
    struct B;
}
