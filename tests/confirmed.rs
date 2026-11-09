use bevy::{
    ecs::{
        change_detection::Tick, component::ComponentId, world::unsafe_world_cell::UnsafeWorldCell,
    },
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

/// The live component runs ahead of the confirmed tick, so once it stops
/// changing, the confirmed values it went through still have to reach the
/// client.
#[test]
fn confirmed_values_reach_the_client_after_the_live_value_stops_changing() {
    let (mut server_app, mut client_app) = confirmed_pair();
    let server_entity = spawn_confirmed(&mut server_app, &mut client_app);
    let tick = next_send_tick(&server_app);
    record(&mut server_app, server_entity, tick + 1, Some(V(1)));
    record(&mut server_app, server_entity, tick + 2, Some(V(2)));
    *server_app.world_mut().get_mut::<V>(server_entity).unwrap() = V(2);

    let mut values = Vec::new();
    for _ in 0..3 {
        exchange(&mut server_app, &mut client_app);
        values.push(client_value(&mut client_app));
    }

    assert_eq!(values, [Some(V(0)), Some(V(1)), Some(V(2))]);
}

/// A confirmed change whose acknowledgment is lost is sent again, though the
/// live value never changed, and is no longer sent once acknowledged.
#[test]
fn confirmed_change_is_resent_until_acknowledged() {
    let (mut server_app, mut client_app) = confirmed_pair();
    let server_entity = spawn_confirmed(&mut server_app, &mut client_app);
    let tick = next_send_tick(&server_app);
    record(&mut server_app, server_entity, tick, Some(V(1)));

    server_app.update();
    server_app.exchange_with_client(&mut client_app);
    client_app.update();
    let lost_acks = client_app
        .world_mut()
        .resource_mut::<ClientMessages>()
        .drain_sent()
        .count();
    let first_receipt = client_change_tick(&mut client_app);
    exchange(&mut server_app, &mut client_app);
    let resent = client_change_tick(&mut client_app);
    exchange(&mut server_app, &mut client_app);

    assert_eq!(lost_acks, 1);
    assert!(
        first_receipt.get() < resent.get(),
        "the lost change is sent again"
    );
    assert_eq!(
        resent,
        client_change_tick(&mut client_app),
        "the acknowledged change is not sent again"
    );
    assert_eq!(client_value(&mut client_app), Some(V(1)));
}

/// A live change the confirmed history has yet to record is not sent: the
/// confirmed value it would carry is the one the client already has.
#[test]
fn live_change_ahead_of_the_confirmed_history_is_not_sent() {
    let (mut server_app, mut client_app) = confirmed_pair();
    let server_entity = spawn_confirmed(&mut server_app, &mut client_app);
    client_app.init_resource::<ClientReplicationStats>();
    let before = client_components_changed(&client_app);
    *server_app.world_mut().get_mut::<V>(server_entity).unwrap() = V(1);

    exchange(&mut server_app, &mut client_app);

    assert_eq!(client_components_changed(&client_app), before);
    assert_eq!(client_value(&mut client_app), Some(V(0)));
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
        let before = client_components_changed(&client_app);
        exchange(&mut server_app, &mut client_app);
        values.push(client_value(&mut client_app));
        components_changed.push(client_components_changed(&client_app) - before);
    }

    assert_eq!(values, [Some(V(0)), None, Some(V(3))]);
    assert_eq!(
        components_changed,
        [0, 1, 1],
        "the unchanged confirmed value is not sent again, then its removal and its re-insertion are, once each"
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

/// A component removed live outside the simulation reaches the confirmed
/// history only at the next tick. When that live removal merges into a
/// withheld removal released the same tick, and the history shows the
/// component re-inserted after it, the confirmed values of the re-insertion
/// and the later removal still reach the client.
#[test]
fn removal_recorded_after_its_merged_removal_is_released_is_sent() {
    let (mut server_app, mut client_app) = confirmed_pair();
    let server_entity = spawn_confirmed(&mut server_app, &mut client_app);
    let tick = next_send_tick(&server_app);
    record(&mut server_app, server_entity, tick, None);
    record(&mut server_app, server_entity, tick + 1, Some(V(5)));
    server_app
        .world_mut()
        .entity_mut(server_entity)
        .remove::<V>()
        .insert(V(5))
        .remove::<V>();

    exchange(&mut server_app, &mut client_app);
    let mut values = vec![client_value(&mut client_app)];
    record(&mut server_app, server_entity, tick + 2, None);
    for _ in 0..2 {
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
        .set(confirmed_v, confirmed_v_ahead, confirmed_v_changed);
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

fn client_change_tick(client_app: &mut App) -> Tick {
    client_app
        .world_mut()
        .query::<Ref<V>>()
        .single(client_app.world())
        .unwrap()
        .last_changed()
}

fn client_components_changed(client_app: &App) -> usize {
    client_app
        .world()
        .resource::<ClientReplicationStats>()
        .components_changed
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

unsafe fn confirmed_v_ahead(
    world: UnsafeWorldCell,
    entity: Entity,
    _component: ComponentId,
    tick: RepliconTick,
) -> ConfirmedAhead {
    let ahead = confirmed_history(world, entity)
        .into_iter()
        .flat_map(|confirmed| &confirmed.0)
        .filter(|&&(at, _)| at > tick.get());
    let mut confirmed = ConfirmedAhead::default();
    for (_, state) in ahead {
        confirmed.present |= state.is_some();
        confirmed.removed |= state.is_none();
    }
    confirmed
}

unsafe fn confirmed_v_changed(
    world: UnsafeWorldCell,
    entity: Entity,
    _component: ComponentId,
    after: RepliconTick,
    until: RepliconTick,
) -> bool {
    confirmed_history(world, entity)
        .into_iter()
        .flat_map(|confirmed| &confirmed.0)
        .any(|&(at, _)| after.get() < at && at <= until.get())
}

fn confirmed_history(world: UnsafeWorldCell<'_>, entity: Entity) -> Option<&ConfirmedV> {
    // SAFETY: the send systems only read `ConfirmedV`.
    world
        .get_entity(entity)
        .ok()
        .and_then(|entity| unsafe { entity.get::<ConfirmedV>() })
}
