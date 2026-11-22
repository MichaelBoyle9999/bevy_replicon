use bevy::{ecs::entity::MapEntities, prelude::*, state::app::StatesPlugin};
use bevy_replicon::{
    prelude::*,
    shared::{
        backend::channels::ServerChannel,
        replication::registry::{ReplicationRegistry, ctx::DespawnCtx},
        server_entity_map::ServerEntityMap,
    },
    test_app::{ServerTestAppExt, TestClientEntity},
};
use serde::{Deserialize, Serialize};
use test_log::test;

#[test]
fn single() {
    let mut server_app = App::new();
    let mut client_app = App::new();
    for app in [&mut server_app, &mut client_app] {
        app.add_plugins((
            MinimalPlugins,
            StatesPlugin,
            RepliconPlugins.set(ServerPlugin::new(PostUpdate)),
        ))
        .finish();
    }

    server_app.connect_client(&mut client_app);

    let server_entity = server_app.world_mut().spawn(Replicated).id();

    server_app.update();
    server_app.exchange_with_client(&mut client_app);
    client_app.update();
    server_app.exchange_with_client(&mut client_app);

    let client_entity = *client_app
        .world()
        .resource::<ServerEntityMap>()
        .to_client()
        .get(&server_entity)
        .unwrap();

    server_app.world_mut().despawn(server_entity);

    server_app.update();
    server_app.exchange_with_client(&mut client_app);
    client_app.update();

    assert!(client_app.world().get_entity(client_entity).is_err());

    let entity_map = client_app.world().resource::<ServerEntityMap>();
    assert!(entity_map.to_client().is_empty());
    assert!(entity_map.to_server().is_empty());
}

#[test]
fn mutation_received_with_the_despawn() {
    let mut server_app = App::new();
    let mut client_app = App::new();
    for app in [&mut server_app, &mut client_app] {
        app.add_plugins((
            MinimalPlugins,
            StatesPlugin,
            RepliconPlugins.set(ServerPlugin::new(PostUpdate)),
        ))
        .replicate::<CounterComponent>()
        .finish();
    }
    client_app
        .world_mut()
        .resource_mut::<ReplicationRegistry>()
        .despawn = mark_despawned;

    server_app.connect_client(&mut client_app);

    let server_entity = server_app
        .world_mut()
        .spawn((Replicated, CounterComponent(0)))
        .id();

    server_app.update();
    server_app.exchange_with_client(&mut client_app);
    client_app.update();
    server_app.exchange_with_client(&mut client_app);

    server_app
        .world_mut()
        .get_mut::<CounterComponent>(server_entity)
        .unwrap()
        .0 = 1;
    server_app.update();
    server_app.world_mut().despawn(server_entity);
    server_app.update();
    server_app.exchange_with_client(&mut client_app);
    client_app.update();

    let (despawned, counter) = client_app
        .world_mut()
        .query::<(Has<Despawned>, &CounterComponent)>()
        .single(client_app.world())
        .unwrap();
    assert!(despawned);
    assert_eq!(
        counter.0, 1,
        "a mutation sent before the despawn must apply even when it arrives with it"
    );

    let entity_map = client_app.world().resource::<ServerEntityMap>();
    assert!(entity_map.to_client().is_empty());
    assert!(entity_map.to_server().is_empty());
}

#[test]
fn mutation_received_after_the_despawn() {
    let mut server_app = App::new();
    let mut client_app = App::new();
    for app in [&mut server_app, &mut client_app] {
        app.add_plugins((
            MinimalPlugins,
            StatesPlugin,
            RepliconPlugins.set(ServerPlugin::new(PostUpdate)),
        ))
        .replicate::<CounterComponent>()
        .finish();
    }
    client_app
        .world_mut()
        .resource_mut::<ReplicationRegistry>()
        .despawn = mark_despawned;

    server_app.connect_client(&mut client_app);

    let server_entity = server_app
        .world_mut()
        .spawn((Replicated, CounterComponent(0)))
        .id();

    server_app.update();
    server_app.exchange_with_client(&mut client_app);
    client_app.update();
    server_app.exchange_with_client(&mut client_app);

    server_app
        .world_mut()
        .get_mut::<CounterComponent>(server_entity)
        .unwrap()
        .0 = 1;
    server_app.update();
    server_app.world_mut().despawn(server_entity);
    server_app.update();

    let mut mutations = Vec::new();
    let mut client_messages = client_app.world_mut().resource_mut::<ClientMessages>();
    for (_, channel_id, message) in server_app
        .world_mut()
        .resource_mut::<ServerMessages>()
        .drain_sent()
    {
        if channel_id == usize::from(ServerChannel::Mutations) {
            mutations.push(message);
        } else {
            client_messages.insert_received(channel_id, message);
        }
    }
    client_app.update();

    let mut counter = client_app
        .world_mut()
        .query_filtered::<&CounterComponent, With<Despawned>>();
    assert_eq!(
        counter.single(client_app.world()).unwrap().0,
        0,
        "the despawn must arrive before the mutation sent ahead of it"
    );

    let mut client_messages = client_app.world_mut().resource_mut::<ClientMessages>();
    for message in mutations {
        client_messages.insert_received(ServerChannel::Mutations, message);
    }
    client_app.update();

    assert_eq!(
        counter.single(client_app.world()).unwrap().0,
        1,
        "a mutation sent before the despawn must apply to the kept entity even when it arrives \
         after the despawn"
    );
}

#[test]
fn references_received_after_the_despawn_map_to_the_kept_entity() {
    let mut server_app = App::new();
    let mut client_app = App::new();
    for app in [&mut server_app, &mut client_app] {
        app.add_plugins((
            MinimalPlugins,
            StatesPlugin,
            RepliconPlugins.set(ServerPlugin::new(PostUpdate)),
        ))
        .replicate::<CounterComponent>()
        .replicate::<MappedComponent>()
        .add_mapped_server_message::<NamedEntity>(Channel::Unreliable)
        .finish();
    }
    client_app
        .world_mut()
        .resource_mut::<ReplicationRegistry>()
        .despawn = mark_despawned;

    server_app.connect_client(&mut client_app);

    let departing = server_app
        .world_mut()
        .spawn((Replicated, CounterComponent(0)))
        .id();
    let referrer = server_app
        .world_mut()
        .spawn((Replicated, MappedComponent(Entity::PLACEHOLDER)))
        .id();

    server_app.update();
    server_app.exchange_with_client(&mut client_app);
    client_app.update();
    server_app.exchange_with_client(&mut client_app);

    server_app
        .world_mut()
        .get_mut::<MappedComponent>(referrer)
        .unwrap()
        .0 = departing;
    server_app.world_mut().write_message(ToClients {
        mode: SendMode::Broadcast,
        message: NamedEntity(departing),
    });
    server_app.update();
    server_app.world_mut().despawn(departing);
    server_app.update();

    let mut delayed = Vec::new();
    let mut client_messages = client_app.world_mut().resource_mut::<ClientMessages>();
    for (_, channel_id, message) in server_app
        .world_mut()
        .resource_mut::<ServerMessages>()
        .drain_sent()
    {
        if channel_id == usize::from(ServerChannel::Updates) {
            client_messages.insert_received(channel_id, message);
        } else {
            delayed.push((channel_id, message));
        }
    }
    client_app.update();
    let kept = client_app
        .world_mut()
        .query_filtered::<Entity, With<Despawned>>()
        .single(client_app.world())
        .expect("the despawn must arrive before the messages sent ahead of it");
    let entities_before = entity_count(&mut client_app);

    let mut client_messages = client_app.world_mut().resource_mut::<ClientMessages>();
    for (channel_id, message) in delayed {
        client_messages.insert_received(channel_id, message);
    }
    client_app.update();

    let reference = client_app
        .world_mut()
        .query::<&MappedComponent>()
        .single(client_app.world())
        .unwrap()
        .0;
    let named: Vec<_> = client_app
        .world()
        .resource::<Messages<NamedEntity>>()
        .iter_current_update_messages()
        .map(|named| named.0)
        .collect();
    assert_eq!(
        (reference, named, entity_count(&mut client_app)),
        (kept, vec![kept], entities_before),
        "a component and a server message sent before the despawn must name the kept entity \
         even when they arrive after the despawn, spawning nothing"
    );
}

#[test]
fn references_to_a_regained_entity_map_to_its_new_client_entity() {
    let mut server_app = App::new();
    let mut client_app = App::new();
    for app in [&mut server_app, &mut client_app] {
        app.add_plugins((
            MinimalPlugins,
            StatesPlugin,
            RepliconPlugins.set(ServerPlugin::new(PostUpdate)),
        ))
        .replicate::<MappedComponent>()
        .add_visibility_filter::<EntityVisibility>()
        .finish();
    }
    client_app
        .world_mut()
        .resource_mut::<ReplicationRegistry>()
        .despawn = mark_despawned;

    server_app.connect_client(&mut client_app);
    let client = **client_app.world().resource::<TestClientEntity>();
    let exchange = |server_app: &mut App, client_app: &mut App| {
        server_app.update();
        server_app.exchange_with_client(client_app);
        client_app.update();
        server_app.exchange_with_client(client_app);
    };

    server_app
        .world_mut()
        .entity_mut(client)
        .insert(EntityVisibility);
    let regained = server_app
        .world_mut()
        .spawn((Replicated, EntityVisibility))
        .id();
    let referrer = server_app
        .world_mut()
        .spawn((Replicated, MappedComponent(Entity::PLACEHOLDER)))
        .id();
    exchange(&mut server_app, &mut client_app);
    server_app
        .world_mut()
        .entity_mut(client)
        .remove::<EntityVisibility>();
    exchange(&mut server_app, &mut client_app);
    server_app
        .world_mut()
        .entity_mut(client)
        .insert(EntityVisibility);
    exchange(&mut server_app, &mut client_app);

    server_app
        .world_mut()
        .get_mut::<MappedComponent>(referrer)
        .unwrap()
        .0 = regained;
    exchange(&mut server_app, &mut client_app);

    let current = client_app
        .world_mut()
        .query_filtered::<Entity, (With<Remote>, Without<Despawned>, Without<MappedComponent>)>()
        .single(client_app.world())
        .expect("the regained entity must reach the client again");
    let reference = client_app
        .world_mut()
        .query::<&MappedComponent>()
        .single(client_app.world())
        .unwrap()
        .0;
    assert_eq!(
        reference, current,
        "a reference to an entity that regained visibility must name its new client entity, \
         not the one its visibility loss kept"
    );
}

#[test]
fn with_relations() {
    let mut server_app = App::new();
    let mut client_app = App::new();
    for app in [&mut server_app, &mut client_app] {
        app.add_plugins((
            MinimalPlugins,
            StatesPlugin,
            RepliconPlugins.set(ServerPlugin::new(PostUpdate)),
        ))
        .finish();
    }

    server_app.connect_client(&mut client_app);

    let server_entity = server_app
        .world_mut()
        .spawn((Replicated, children![Replicated]))
        .id();

    server_app.update();
    server_app.exchange_with_client(&mut client_app);
    client_app.update();
    server_app.exchange_with_client(&mut client_app);

    let mut remote = client_app.world_mut().query::<&Remote>();
    assert_eq!(remote.iter(client_app.world()).len(), 2);

    server_app.world_mut().despawn(server_entity);

    server_app.update();
    server_app.exchange_with_client(&mut client_app);
    client_app.update();

    assert_eq!(remote.iter(client_app.world()).len(), 0);
}

#[test]
fn after_spawn() {
    let mut server_app = App::new();
    let mut client_app = App::new();
    for app in [&mut server_app, &mut client_app] {
        app.add_plugins((
            MinimalPlugins,
            StatesPlugin,
            RepliconPlugins.set(ServerPlugin::new(PostUpdate)),
        ))
        .replicate::<TestComponent>()
        .finish();
    }

    server_app.connect_client(&mut client_app);

    // Insert and remove `Replicated` to trigger spawn and despawn for client at the same time.
    server_app
        .world_mut()
        .spawn((Replicated, TestComponent))
        .remove::<Replicated>();

    server_app.update();

    let mut messages = server_app.world_mut().resource_mut::<ServerMessages>();
    assert_eq!(
        messages.drain_sent().count(),
        0,
        "client shouldn't receive anything for a despawned entity"
    );
}

#[test]
fn signature() {
    let mut server_app = App::new();
    let mut client_app = App::new();
    for app in [&mut server_app, &mut client_app] {
        app.add_plugins((
            MinimalPlugins,
            StatesPlugin,
            RepliconPlugins.set(ServerPlugin::new(PostUpdate)),
        ))
        .finish();
    }

    server_app.connect_client(&mut client_app);

    let server_entity = server_app
        .world_mut()
        .spawn((Replicated, Signature::from(0)))
        .id();
    let client_entity = client_app
        .world_mut()
        .spawn((Replicated, Signature::from(0)))
        .id();

    server_app.update();
    server_app.exchange_with_client(&mut client_app);
    client_app.update();
    server_app.exchange_with_client(&mut client_app);

    server_app.world_mut().despawn(server_entity);

    server_app.update();
    server_app.exchange_with_client(&mut client_app);
    client_app.update();

    assert!(client_app.world().get_entity(client_entity).is_err());
}

#[test]
fn signature_with_hierarchy() {
    let mut server_app = App::new();
    let mut client_app = App::new();
    for app in [&mut server_app, &mut client_app] {
        app.add_plugins((
            MinimalPlugins,
            StatesPlugin,
            RepliconPlugins.set(ServerPlugin::new(PostUpdate)),
        ))
        .replicate::<TestComponent>()
        .finish();
    }

    server_app.connect_client(&mut client_app);

    let client_parent = client_app
        .world_mut()
        .spawn((Replicated, Signature::from(0)))
        .id();
    let client_child = client_app
        .world_mut()
        .spawn((Replicated, Signature::from(1), ChildOf(client_parent)))
        .id();

    let server_parent = server_app
        .world_mut()
        .spawn((Replicated, Signature::from(0)))
        .id();
    server_app
        .world_mut()
        .spawn((Replicated, Signature::from(1), ChildOf(server_parent)));

    server_app.update();
    server_app.exchange_with_client(&mut client_app);
    client_app.update();
    server_app.exchange_with_client(&mut client_app);

    assert!(client_app.world().get_entity(client_parent).is_ok());
    assert!(client_app.world().get_entity(client_child).is_ok());

    server_app.world_mut().despawn(server_parent);

    server_app.update();
    server_app.exchange_with_client(&mut client_app);
    client_app.update();

    assert!(client_app.world().get_entity(client_parent).is_err());
    assert!(client_app.world().get_entity(client_child).is_err());

    let server_parent = server_app
        .world_mut()
        .spawn((Replicated, Signature::from(0)))
        .id();
    server_app
        .world_mut()
        .spawn((Replicated, Signature::from(1), ChildOf(server_parent)));

    server_app.update();
    server_app.exchange_with_client(&mut client_app);
    client_app.update();

    let mut remote = client_app.world_mut().query::<&Remote>();
    assert_eq!(
        remote.iter(client_app.world()).count(),
        2,
        "entities should be replicated as new due to removal from the signature map"
    );
}

#[test]
fn hidden_entity() {
    let mut server_app = App::new();
    let mut client_app = App::new();
    for app in [&mut server_app, &mut client_app] {
        app.add_plugins((
            MinimalPlugins,
            StatesPlugin,
            RepliconPlugins.set(ServerPlugin::new(PostUpdate)),
        ))
        .add_visibility_filter::<EntityVisibility>()
        .finish();
    }

    server_app.connect_client(&mut client_app);

    let server_entity = server_app
        .world_mut()
        .spawn((Replicated, EntityVisibility))
        .id();

    server_app.update();
    server_app.exchange_with_client(&mut client_app);
    client_app.update();
    server_app.exchange_with_client(&mut client_app);

    server_app.world_mut().entity_mut(server_entity).despawn();

    server_app.update();

    let mut messages = server_app.world_mut().resource_mut::<ServerMessages>();
    assert_eq!(
        messages.drain_sent().count(),
        0,
        "client shouldn't receive anything for a hidden entity"
    );
}

#[test]
fn visibility_lose() {
    let mut server_app = App::new();
    let mut client_app = App::new();
    for app in [&mut server_app, &mut client_app] {
        app.add_plugins((
            MinimalPlugins,
            StatesPlugin,
            RepliconPlugins.set(ServerPlugin::new(PostUpdate)),
        ))
        .add_visibility_filter::<EntityVisibility>()
        .finish();
    }

    server_app.connect_client(&mut client_app);

    let client = **client_app.world().resource::<TestClientEntity>();
    server_app
        .world_mut()
        .entity_mut(client)
        .insert(EntityVisibility);

    server_app.world_mut().spawn((Replicated, EntityVisibility));

    server_app.update();
    server_app.exchange_with_client(&mut client_app);
    client_app.update();
    server_app.exchange_with_client(&mut client_app);

    let mut remote = client_app.world_mut().query::<&Remote>();
    assert_eq!(remote.iter(client_app.world()).len(), 1);

    server_app
        .world_mut()
        .entity_mut(client)
        .remove::<EntityVisibility>();

    server_app.update();
    server_app.exchange_with_client(&mut client_app);
    client_app.update();

    assert_eq!(remote.iter(client_app.world()).len(), 0);
}

#[test]
fn visibility_lose_and_regain_in_one_receive() {
    let mut server_app = App::new();
    let mut client_app = App::new();
    for app in [&mut server_app, &mut client_app] {
        app.add_plugins((
            MinimalPlugins,
            StatesPlugin,
            RepliconPlugins.set(ServerPlugin::new(PostUpdate)),
        ))
        .add_visibility_filter::<EntityVisibility>()
        .finish();
    }

    server_app.connect_client(&mut client_app);

    let client = **client_app.world().resource::<TestClientEntity>();
    server_app
        .world_mut()
        .entity_mut(client)
        .insert(EntityVisibility);

    let server_entity = server_app
        .world_mut()
        .spawn((Replicated, EntityVisibility))
        .id();

    server_app.update();
    server_app.exchange_with_client(&mut client_app);
    client_app.update();
    server_app.exchange_with_client(&mut client_app);

    server_app
        .world_mut()
        .entity_mut(client)
        .remove::<EntityVisibility>();
    server_app.update();
    server_app
        .world_mut()
        .entity_mut(client)
        .insert(EntityVisibility);
    server_app.update();
    server_app.exchange_with_client(&mut client_app);
    client_app.update();

    let client_entity = client_app
        .world_mut()
        .query_filtered::<Entity, With<Remote>>()
        .single(client_app.world())
        .unwrap();
    let entity_map = client_app.world().resource::<ServerEntityMap>();
    assert_eq!(
        entity_map.to_client().get(&server_entity),
        Some(&client_entity),
        "the entity respawned after the despawn in the same receive must stay mapped"
    );
}

#[test]
fn visibility_lose_with_component_scope() {
    let mut server_app = App::new();
    let mut client_app = App::new();
    for app in [&mut server_app, &mut client_app] {
        app.add_plugins((
            MinimalPlugins,
            StatesPlugin,
            RepliconPlugins.set(ServerPlugin::new(PostUpdate)),
        ))
        .replicate::<TestComponent>()
        .add_visibility_filter::<EntityVisibility>()
        .add_visibility_filter::<ComponentVisibility>()
        .finish();
    }

    server_app.connect_client(&mut client_app);

    let client = **client_app.world().resource::<TestClientEntity>();
    server_app
        .world_mut()
        .entity_mut(client)
        .insert((EntityVisibility, ComponentVisibility));

    server_app.world_mut().spawn((
        Replicated,
        TestComponent,
        EntityVisibility,
        ComponentVisibility,
    ));

    server_app.update();
    server_app.exchange_with_client(&mut client_app);
    client_app.update();
    server_app.exchange_with_client(&mut client_app);

    let mut components = client_app.world_mut().query::<&TestComponent>();
    assert_eq!(components.iter(client_app.world()).len(), 1);

    server_app
        .world_mut()
        .entity_mut(client)
        .remove::<(EntityVisibility, ComponentVisibility)>();

    server_app.update();
    server_app.exchange_with_client(&mut client_app);
    client_app.update();

    assert_eq!(components.iter(client_app.world()).len(), 0);
}

#[test]
fn with_visibility_gain_and_signature() {
    let mut server_app = App::new();
    let mut client_app = App::new();
    for app in [&mut server_app, &mut client_app] {
        app.add_plugins((
            MinimalPlugins,
            StatesPlugin,
            RepliconPlugins.set(ServerPlugin::new(PostUpdate)),
        ))
        .add_visibility_filter::<EntityVisibility>()
        .finish();
    }

    server_app.connect_client(&mut client_app);

    // Remove visibility filter to make the entity visible and unreplicate.
    server_app
        .world_mut()
        .spawn((Replicated, EntityVisibility, Signature::from(0)))
        .remove::<(Replicated, EntityVisibility)>();

    server_app.update();

    let mut messages = server_app.world_mut().resource_mut::<ServerMessages>();
    assert_eq!(messages.drain_sent().count(), 0);
}

#[derive(Component, Deserialize, Serialize)]
struct TestComponent;

#[derive(Component, Deserialize, Serialize)]
struct CounterComponent(u32);

#[derive(Component)]
struct Despawned;

fn mark_despawned(_ctx: &DespawnCtx, mut entity: EntityWorldMut) {
    entity.insert(Despawned);
}

#[derive(Component)]
#[component(immutable)]
struct EntityVisibility;

impl VisibilityFilter for EntityVisibility {
    type ClientComponent = Self;
    type Scope = Entity;

    fn is_visible(&self, _client: Entity, component: Option<&Self::ClientComponent>) -> bool {
        component.is_some()
    }
}

#[derive(Component)]
#[component(immutable)]
struct ComponentVisibility;

impl VisibilityFilter for ComponentVisibility {
    type ClientComponent = Self;
    type Scope = SingleComponent<TestComponent>;

    fn is_visible(&self, _client: Entity, component: Option<&Self::ClientComponent>) -> bool {
        component.is_some()
    }
}

fn entity_count(app: &mut App) -> usize {
    app.world_mut()
        .query::<EntityRef>()
        .iter(app.world())
        .count()
}

#[derive(Component, Deserialize, Serialize, MapEntities)]
struct MappedComponent(#[entities] Entity);

#[derive(Message, Deserialize, Serialize, MapEntities, Clone, Copy)]
struct NamedEntity(#[entities] Entity);
