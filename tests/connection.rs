use bevy::prelude::*;
use bevy_replicon::{
    core::channels::ReplicationChannel, prelude::*, server::server_tick::ServerTick,
    test_app::ServerTestAppExt,
};

#[test]
fn client_to_server() {
    let mut server_app = App::new();
    let mut client_app = App::new();
    for app in [&mut server_app, &mut client_app] {
        app.add_plugins((MinimalPlugins, RepliconPlugins));
        app.update();
    }

    const MESSAGES: &[&[u8]] = &[&[0], &[1]];
    const CLIENT_ID: ClientId = ClientId::new(0);

    let mut client = client_app.world_mut().resource_mut::<RepliconClient>();
    client.set_status(RepliconClientStatus::Connected {
        client_id: Some(CLIENT_ID),
    });
    for &message in MESSAGES {
        client.send(ReplicationChannel::Init, message);
    }

    let mut server = server_app.world_mut().resource_mut::<RepliconServer>();
    server.set_running(true);

    for (channel_id, message) in client.drain_sent() {
        server.insert_received(CLIENT_ID, channel_id, message);
    }

    let messages: Vec<_> = server
        .receive(ReplicationChannel::Init)
        .map(|(_, message)| message)
        .collect();
    assert_eq!(messages, MESSAGES);
}

#[test]
fn server_to_client() {
    let mut server_app = App::new();
    let mut client_app = App::new();
    for app in [&mut server_app, &mut client_app] {
        app.add_plugins((MinimalPlugins, RepliconPlugins));
        app.update();
    }

    const MESSAGES: &[&[u8]] = &[&[0], &[1]];
    const CLIENT_ID: ClientId = ClientId::new(0);

    let mut server = server_app.world_mut().resource_mut::<RepliconServer>();
    server.set_running(true);
    for &message in MESSAGES {
        server.send(CLIENT_ID, ReplicationChannel::Init, message);
    }

    let mut client = client_app.world_mut().resource_mut::<RepliconClient>();
    client.set_status(RepliconClientStatus::Connected {
        client_id: Some(CLIENT_ID),
    });

    for (_, channel_id, message) in server.drain_sent() {
        client.insert_received(channel_id, message);
    }

    let messages: Vec<_> = client.receive(ReplicationChannel::Init).collect();
    assert_eq!(messages, MESSAGES);
}

#[test]
fn connect_disconnect() {
    let mut server_app = App::new();
    let mut client_app = App::new();
    for app in [&mut server_app, &mut client_app] {
        app.add_plugins((
            MinimalPlugins,
            RepliconPlugins.set(ServerPlugin {
                tick_policy: TickPolicy::EveryFrame,
                ..Default::default()
            }),
        ));
    }

    server_app.connect_client(&mut client_app);

    let replicated_clients = server_app.world().resource::<ReplicatedClients>();
    assert_eq!(replicated_clients.len(), 1);

    server_app.disconnect_client(&mut client_app);

    let replicated_clients = server_app.world().resource::<ReplicatedClients>();
    assert!(replicated_clients.is_empty());
}

#[test]
fn client_cleanup_on_disconnect() {
    let mut app = App::new();
    app.add_plugins((
        MinimalPlugins,
        RepliconPlugins.set(ServerPlugin {
            tick_policy: TickPolicy::EveryFrame,
            ..Default::default()
        }),
    ));

    app.update();

    let mut client = app.world_mut().resource_mut::<RepliconClient>();
    client.set_status(RepliconClientStatus::Connected { client_id: None });

    client.send(ReplicationChannel::Init, Vec::new());
    client.insert_received(ReplicationChannel::Init, Vec::new());

    client.set_status(RepliconClientStatus::Disconnected);

    assert_eq!(client.drain_sent().count(), 0);
    assert_eq!(client.receive(ReplicationChannel::Init).count(), 0);

    app.update();
}

#[test]
fn server_cleanup_on_stop() {
    let mut app = App::new();
    app.add_plugins((
        MinimalPlugins,
        RepliconPlugins.set(ServerPlugin {
            tick_policy: TickPolicy::EveryFrame,
            ..Default::default()
        }),
    ));

    app.update();

    let mut server = app.world_mut().resource_mut::<RepliconServer>();
    server.set_running(true);

    const DUMMY_CLIENT_ID: ClientId = ClientId::new(1);
    server.send(DUMMY_CLIENT_ID, ReplicationChannel::Init, Vec::new());
    server.insert_received(DUMMY_CLIENT_ID, ReplicationChannel::Init, Vec::new());

    server.set_running(false);

    assert_eq!(server.drain_sent().count(), 0);
    assert_eq!(server.receive(ReplicationChannel::Init).count(), 0);

    app.update();

    assert_eq!(app.world().resource::<ServerTick>().get(), 0);
}

#[test]
fn client_disconnected() {
    let mut app = App::new();
    app.add_plugins((
        MinimalPlugins,
        RepliconPlugins.set(ServerPlugin {
            tick_policy: TickPolicy::EveryFrame,
            ..Default::default()
        }),
    ));

    app.update();

    let mut client = app.world_mut().resource_mut::<RepliconClient>();

    client.send(ReplicationChannel::Init, Vec::new());
    client.insert_received(ReplicationChannel::Init, Vec::new());

    assert_eq!(client.drain_sent().count(), 0);
    assert_eq!(client.receive(ReplicationChannel::Init).count(), 0);

    app.update();
}

#[test]
fn server_inactive() {
    let mut app = App::new();
    app.add_plugins((
        MinimalPlugins,
        RepliconPlugins.set(ServerPlugin {
            tick_policy: TickPolicy::EveryFrame,
            ..Default::default()
        }),
    ));

    app.update();

    let mut server = app.world_mut().resource_mut::<RepliconServer>();

    const DUMMY_CLIENT_ID: ClientId = ClientId::new(1);

    server.send(DUMMY_CLIENT_ID, ReplicationChannel::Init, Vec::new());
    server.insert_received(DUMMY_CLIENT_ID, ReplicationChannel::Init, Vec::new());

    assert_eq!(server.drain_sent().count(), 0);
    assert_eq!(server.receive(ReplicationChannel::Init).count(), 0);

    app.update();

    assert_eq!(app.world().resource::<ServerTick>().get(), 0);
}
