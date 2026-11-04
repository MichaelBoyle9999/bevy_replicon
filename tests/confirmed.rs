use bevy::{
    ecs::{component::ComponentId, world::unsafe_world_cell::UnsafeWorldCell},
    prelude::*,
    ptr::Ptr,
    state::app::StatesPlugin,
};
use bevy_replicon::{
    prelude::*, server::server_tick::ServerTick, shared::replicon_tick::RepliconTick,
    test_app::ServerTestAppExt,
};
use serde::{Deserialize, Serialize};
use test_log::test;

/// With a confirmed source, the live component runs ahead of the replicated
/// [`ServerTick`]: a live removal reaches clients only once the confirmed state
/// at `ServerTick` no longer has the component.
#[test]
fn removal_withheld_until_confirmed_absent() {
    let (mut server_app, mut client_app) = confirmed_pair();
    let server_entity = spawn_confirmed(&mut server_app, &mut client_app);
    let tick = next_send_tick(&server_app);
    record(&mut server_app, server_entity, tick + 1, None);
    server_app
        .world_mut()
        .entity_mut(server_entity)
        .remove::<V>();

    exchange(&mut server_app, &mut client_app);
    assert_eq!(
        client_value(&mut client_app),
        Some(V(0)),
        "the confirmed state still has the component"
    );

    exchange(&mut server_app, &mut client_app);
    assert_eq!(client_value(&mut client_app), None);
}

#[test]
fn removal_without_confirmed_history_is_sent_at_once() {
    let (mut server_app, mut client_app) = confirmed_pair();
    let server_entity = server_app.world_mut().spawn((Replicated, V(0))).id();
    exchange(&mut server_app, &mut client_app);

    server_app
        .world_mut()
        .entity_mut(server_entity)
        .remove::<V>();
    exchange(&mut server_app, &mut client_app);

    assert_eq!(client_value(&mut client_app), None);
}

#[test]
fn removed_and_reinserted_without_confirmed_history_is_not_sent() {
    let (mut server_app, mut client_app) = confirmed_pair();
    let server_entity = server_app.world_mut().spawn((Replicated, V(0))).id();
    exchange(&mut server_app, &mut client_app);

    server_app
        .world_mut()
        .entity_mut(server_entity)
        .remove::<V>()
        .insert(V(1));
    exchange(&mut server_app, &mut client_app);

    assert_eq!(client_value(&mut client_app), Some(V(1)));
}

/// The confirmed values of a component removed live keep replicating up to the
/// tick before its confirmed removal.
#[test]
fn confirmed_values_before_a_withheld_removal_reach_the_client() {
    let (mut server_app, mut client_app) = confirmed_pair();
    let server_entity = spawn_confirmed(&mut server_app, &mut client_app);
    let tick = next_send_tick(&server_app);
    record(&mut server_app, server_entity, tick, Some(V(1)));
    record(&mut server_app, server_entity, tick + 1, Some(V(2)));
    record(&mut server_app, server_entity, tick + 2, None);
    server_app
        .world_mut()
        .entity_mut(server_entity)
        .remove::<V>();

    let mut values = Vec::new();
    for _ in 0..3 {
        exchange(&mut server_app, &mut client_app);
        values.push(client_value(&mut client_app));
    }

    assert_eq!(values, [Some(V(1)), Some(V(2)), None]);
}

/// A component removed and re-inserted live while its removal is still
/// withheld reaches the client as a removal and then an insertion, at the
/// confirmed ticks.
#[test]
fn reinsertion_within_the_lead_follows_the_confirmed_removal() {
    let (mut server_app, mut client_app) = confirmed_pair();
    let server_entity = spawn_confirmed(&mut server_app, &mut client_app);
    let tick = next_send_tick(&server_app);
    record(&mut server_app, server_entity, tick + 1, None);
    record(&mut server_app, server_entity, tick + 2, Some(V(3)));
    client_app.init_resource::<ClientReplicationStats>();
    server_app
        .world_mut()
        .entity_mut(server_entity)
        .remove::<V>()
        .insert(V(3));

    let mut values = Vec::new();
    let mut components_changed = Vec::new();
    for _ in 0..3 {
        let before = client_app
            .world()
            .resource::<ClientReplicationStats>()
            .components_changed;
        exchange(&mut server_app, &mut client_app);
        values.push(client_value(&mut client_app));
        let after = client_app
            .world()
            .resource::<ClientReplicationStats>()
            .components_changed;
        components_changed.push(after - before);
    }

    assert_eq!(values, [Some(V(0)), None, Some(V(3))]);
    assert_eq!(
        components_changed,
        [1, 1, 1],
        "each tick replicates the component once: its confirmed value, its removal, its re-insertion"
    );
}

/// A component inserted and removed live within the lead reaches the client
/// only for the confirmed ticks it existed in.
#[test]
fn component_inserted_and_removed_within_the_lead_replicates_its_confirmed_span() {
    let (mut server_app, mut client_app) = confirmed_pair();
    let server_entity = server_app
        .world_mut()
        .spawn((Replicated, ConfirmedV(Vec::new())))
        .id();
    exchange(&mut server_app, &mut client_app);
    let tick = next_send_tick(&server_app);
    record(&mut server_app, server_entity, tick + 1, Some(V(1)));
    record(&mut server_app, server_entity, tick + 2, None);
    server_app
        .world_mut()
        .entity_mut(server_entity)
        .insert(V(1))
        .remove::<V>();

    let mut values = Vec::new();
    for _ in 0..3 {
        exchange(&mut server_app, &mut client_app);
        values.push(client_value(&mut client_app));
    }

    assert_eq!(values, [None, Some(V(1)), None]);
}

/// A removal recorded again after a confirmed re-insertion is sent too, even
/// though the live removals were buffered as one.
#[test]
fn removal_after_a_confirmed_reinsertion_is_sent() {
    let (mut server_app, mut client_app) = confirmed_pair();
    let server_entity = spawn_confirmed(&mut server_app, &mut client_app);
    let tick = next_send_tick(&server_app);
    record(&mut server_app, server_entity, tick, None);
    record(&mut server_app, server_entity, tick + 1, Some(V(5)));
    record(&mut server_app, server_entity, tick + 2, None);
    server_app
        .world_mut()
        .entity_mut(server_entity)
        .remove::<V>()
        .insert(V(5))
        .remove::<V>();

    let mut values = Vec::new();
    for _ in 0..3 {
        exchange(&mut server_app, &mut client_app);
        values.push(client_value(&mut client_app));
    }

    assert_eq!(values, [None, Some(V(5)), None]);
}

/// A withheld removal that the confirmed history stops recording, such as
/// after a host resimulation undid it, never reaches the client.
#[test]
fn undone_removal_is_not_sent() {
    let (mut server_app, mut client_app) = confirmed_pair();
    let server_entity = spawn_confirmed(&mut server_app, &mut client_app);
    let tick = next_send_tick(&server_app);
    record(&mut server_app, server_entity, tick + 1, None);
    server_app
        .world_mut()
        .entity_mut(server_entity)
        .remove::<V>();
    exchange(&mut server_app, &mut client_app);

    let mut entity = server_app.world_mut().entity_mut(server_entity);
    entity.insert(V(0));
    entity.get_mut::<ConfirmedV>().unwrap().0.pop();
    for _ in 0..3 {
        exchange(&mut server_app, &mut client_app);
        assert_eq!(client_value(&mut client_app), Some(V(0)));
    }
}

fn confirmed_pair() -> (App, App) {
    let mut server_app = App::new();
    let mut client_app = App::new();
    for app in [&mut server_app, &mut client_app] {
        app.add_plugins((
            MinimalPlugins,
            StatesPlugin,
            RepliconPlugins.set(ServerPlugin::new(PostUpdate)),
        ))
        .replicate::<V>()
        .finish();
    }
    server_app
        .world_mut()
        .resource_mut::<ConfirmedReplicationSource>()
        .set(confirmed_v, confirmed_v_removed_after);
    server_app.connect_client(&mut client_app);
    (server_app, client_app)
}

/// Spawns a [`V`] confirmed present since tick 0 and replicates it.
fn spawn_confirmed(server_app: &mut App, client_app: &mut App) -> Entity {
    let entity = server_app
        .world_mut()
        .spawn((Replicated, V(0), ConfirmedV(vec![(0, Some(V(0)))])))
        .id();
    exchange(server_app, client_app);
    entity
}

fn next_send_tick(server_app: &App) -> u32 {
    server_app.world().resource::<ServerTick>().get() + 1
}

fn record(server_app: &mut App, entity: Entity, tick: u32, state: Option<V>) {
    server_app
        .world_mut()
        .get_mut::<ConfirmedV>(entity)
        .unwrap()
        .0
        .push((tick, state));
}

fn exchange(server_app: &mut App, client_app: &mut App) {
    server_app.update();
    server_app.exchange_with_client(client_app);
    client_app.update();
    server_app.exchange_with_client(client_app);
}

fn client_value(client_app: &mut App) -> Option<V> {
    client_app
        .world_mut()
        .query::<&V>()
        .iter(client_app.world())
        .next()
        .copied()
}

#[derive(Component, Clone, Copy, Debug, PartialEq, Deserialize, Serialize)]
struct V(u32);

/// The confirmed history of [`V`]: each entry sets its state from that tick
/// on, `None` meaning removed.
#[derive(Component)]
struct ConfirmedV(Vec<(u32, Option<V>)>);

unsafe fn confirmed_v<'w>(
    world: UnsafeWorldCell<'w>,
    entity: Entity,
    _component: ComponentId,
    tick: RepliconTick,
) -> ConfirmedLookup<'w> {
    let Some(confirmed) = confirmed_history(world, entity) else {
        return ConfirmedLookup::Live;
    };
    match confirmed.0.iter().rev().find(|&&(at, _)| at <= tick.get()) {
        Some((_, Some(value))) => ConfirmedLookup::Confirmed(Ptr::from(value)),
        Some((_, None)) => ConfirmedLookup::Removed,
        None => ConfirmedLookup::Unconfirmed,
    }
}

unsafe fn confirmed_v_removed_after(
    world: UnsafeWorldCell,
    entity: Entity,
    _component: ComponentId,
    tick: RepliconTick,
) -> bool {
    confirmed_history(world, entity).is_some_and(|confirmed| {
        confirmed
            .0
            .iter()
            .any(|&(at, state)| at > tick.get() && state.is_none())
    })
}

fn confirmed_history(world: UnsafeWorldCell<'_>, entity: Entity) -> Option<&ConfirmedV> {
    // SAFETY: the send systems only read `ConfirmedV`.
    world
        .get_entity(entity)
        .ok()
        .and_then(|entity| unsafe { entity.get::<ConfirmedV>() })
}
