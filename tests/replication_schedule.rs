use core::time::Duration;

use bevy::{
    ecs::{entity::MapEntities, schedule::ScheduleLabel},
    prelude::*,
    state::app::StatesPlugin,
    time::TimeUpdateStrategy,
};
use bevy_replicon::{
    client::server_mutate_ticks::MutateTickReceived,
    prelude::*,
    server::server_tick::ServerTick,
    shared::{replication::track_mutate_messages::TrackAppExt, server_entity_map::ServerEntityMap},
    test_app::ServerTestAppExt,
};
use serde::{Deserialize, Serialize};
use test_log::test;

const TICKS_PER_FRAME: u32 = 3;

#[test]
fn fixed_replication_sends_every_tick_of_a_frame() {
    let ticks = ticks_received_in_one_frame(ServerPlugin {
        replication_schedule: FixedPostUpdate.intern(),
        ..ServerPlugin::new(FixedPostUpdate)
    });
    assert_eq!(ticks.len(), TICKS_PER_FRAME as usize);
    assert!(
        ticks.windows(2).all(|pair| pair[1] == pair[0] + 1),
        "each tick of the frame must arrive stamped with its own tick: {ticks:?}"
    );
}

#[test]
fn frame_replication_sends_only_the_last_tick_of_a_frame() {
    let ticks = ticks_received_in_one_frame(ServerPlugin::new(FixedPostUpdate));
    assert_eq!(ticks.len(), 1);
}

fn ticks_received_in_one_frame(server_plugin: ServerPlugin) -> Vec<RepliconTick> {
    let mut server_app = App::new();
    let mut client_app = App::new();
    server_app.add_plugins((
        MinimalPlugins,
        StatesPlugin,
        RepliconPlugins.set(server_plugin),
    ));
    client_app.add_plugins((MinimalPlugins, StatesPlugin, RepliconPlugins));
    for app in [&mut server_app, &mut client_app] {
        app.insert_resource(TimeUpdateStrategy::ManualDuration(Duration::ZERO))
            .track_mutate_messages()
            .finish();
    }

    server_app.connect_client(&mut client_app);
    client_app
        .world_mut()
        .resource_mut::<Messages<MutateTickReceived>>()
        .clear();

    let timestep = server_app.world().resource::<Time<Fixed>>().timestep();
    server_app.insert_resource(TimeUpdateStrategy::ManualDuration(
        timestep * TICKS_PER_FRAME,
    ));
    server_app.update();
    server_app.exchange_with_client(&mut client_app);
    client_app.update();

    let mut ticks: Vec<_> = client_app
        .world_mut()
        .resource_mut::<Messages<MutateTickReceived>>()
        .drain()
        .map(|received| received.tick)
        .collect();
    ticks.sort();
    ticks
}

#[test]
fn fixed_replication_maps_a_message_to_an_entity_spawned_after_the_frames_ticks() {
    let mut server_app = App::new();
    let mut client_app = App::new();
    server_app.add_plugins((
        MinimalPlugins,
        StatesPlugin,
        RepliconPlugins.set(ServerPlugin {
            replication_schedule: FixedPostUpdate.intern(),
            ..ServerPlugin::new(FixedPostUpdate)
        }),
    ));
    client_app.add_plugins((MinimalPlugins, StatesPlugin, RepliconPlugins));
    for app in [&mut server_app, &mut client_app] {
        app.insert_resource(TimeUpdateStrategy::ManualDuration(Duration::ZERO))
            .add_mapped_server_message::<WithEntity>(Channel::Ordered)
            .finish();
    }
    server_app.add_systems(Update, spawn_and_announce.run_if(run_once));

    server_app.connect_client(&mut client_app);
    let timestep = server_app.world().resource::<Time<Fixed>>().timestep();
    server_app.insert_resource(TimeUpdateStrategy::ManualDuration(timestep));
    for _ in 0..2 {
        server_app.update();
        server_app.exchange_with_client(&mut client_app);
        client_app.update();
    }

    let server_entity = server_app
        .world_mut()
        .query_filtered::<Entity, With<Replicated>>()
        .single(server_app.world())
        .unwrap();
    let client_entity = client_app
        .world()
        .resource::<ServerEntityMap>()
        .to_client()
        .get(&server_entity)
        .copied();
    let mapped: Vec<_> = client_app
        .world_mut()
        .resource_mut::<Messages<WithEntity>>()
        .drain()
        .map(|message| Some(message.0))
        .collect();
    assert!(client_entity.is_some());
    assert_eq!(
        mapped,
        [client_entity],
        "a message written after the frame's ticks must wait for the tick that replicates the \
         entity it names"
    );
}

fn spawn_and_announce(mut commands: Commands, mut messages: MessageWriter<ToClients<WithEntity>>) {
    let entity = commands.spawn(Replicated).id();
    messages.write(ToClients {
        mode: SendMode::Broadcast,
        message: WithEntity(entity),
    });
}

#[derive(Message, Serialize, Deserialize, MapEntities)]
struct WithEntity(#[entities] Entity);

#[test]
fn fixed_replication_sends_a_message_written_before_a_ticks_send_with_that_tick() {
    let mut server_app = App::new();
    let mut client_app = App::new();
    server_app.add_plugins((
        MinimalPlugins,
        StatesPlugin,
        RepliconPlugins.set(ServerPlugin {
            replication_schedule: FixedPostUpdate.intern(),
            ..ServerPlugin::new(FixedPostUpdate)
        }),
    ));
    client_app.add_plugins((MinimalPlugins, StatesPlugin, RepliconPlugins));
    for app in [&mut server_app, &mut client_app] {
        app.insert_resource(TimeUpdateStrategy::ManualDuration(Duration::ZERO))
            .add_server_message::<TickMessage>(Channel::Ordered)
            .finish();
    }
    server_app.add_systems(
        FixedPostUpdate,
        write_tick_message
            .after(ServerSystems::IncrementTick)
            .before(ServerSystems::Send),
    );

    server_app.connect_client(&mut client_app);
    let timestep = server_app.world().resource::<Time<Fixed>>().timestep();
    server_app.insert_resource(TimeUpdateStrategy::ManualDuration(timestep));
    server_app.update();
    server_app.exchange_with_client(&mut client_app);
    client_app.update();

    let tick = server_app.world().resource::<ServerTick>().get();
    let received: Vec<_> = client_app
        .world_mut()
        .resource_mut::<Messages<TickMessage>>()
        .drain()
        .map(|message| message.0)
        .collect();
    assert_eq!(
        received,
        [tick],
        "a message written in a tick before its send must go out with that tick's send"
    );
}

fn write_tick_message(tick: Res<ServerTick>, mut messages: MessageWriter<ToClients<TickMessage>>) {
    messages.write(ToClients {
        mode: SendMode::Broadcast,
        message: TickMessage(tick.get()),
    });
}

#[derive(Message, Serialize, Deserialize)]
struct TickMessage(u32);

#[test]
fn fixed_replication_sends_an_independent_message_in_a_frame_without_a_tick() {
    let mut server_app = App::new();
    let mut client_app = App::new();
    server_app.add_plugins((
        MinimalPlugins,
        StatesPlugin,
        RepliconPlugins.set(ServerPlugin {
            replication_schedule: FixedPostUpdate.intern(),
            ..ServerPlugin::new(FixedPostUpdate)
        }),
    ));
    client_app.add_plugins((MinimalPlugins, StatesPlugin, RepliconPlugins));
    for app in [&mut server_app, &mut client_app] {
        app.insert_resource(TimeUpdateStrategy::ManualDuration(Duration::ZERO))
            .add_server_message::<TickMessage>(Channel::Ordered)
            .make_message_independent::<TickMessage>()
            .finish();
    }

    server_app.connect_client(&mut client_app);
    server_app.add_systems(Update, write_tick_message.run_if(run_once));
    let tick = server_app.world().resource::<ServerTick>().get();
    server_app.update();
    server_app.exchange_with_client(&mut client_app);
    client_app.update();

    assert_eq!(
        server_app.world().resource::<ServerTick>().get(),
        tick,
        "the frame must run no tick"
    );
    let received: Vec<_> = client_app
        .world_mut()
        .resource_mut::<Messages<TickMessage>>()
        .drain()
        .map(|message| message.0)
        .collect();
    assert_eq!(
        received,
        [tick],
        "an independent message must go out in the frame it is written, so that one written \
         before a disconnect request, such as a protocol mismatch, reaches the client first"
    );
}
