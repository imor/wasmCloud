use crate::common::{await_actor_count, await_provider_count, par_from_file};
use actix_rt::time::delay_for;
use provider_archive::ProviderArchive;
use std::collections::HashMap;
use std::time::Duration;
use wasmcloud_host::{Actor, HostBuilder, NativeCapability};
use wasmcloud_host::{Host, Result};

// Start two hosts, A and B. Host A contains an actor
// and host B contains a provider. Set a link via host B's
// API and then invoke the provider's running HTTP endpoint
// to ensure the RPC link between actor and provider works
pub(crate) async fn distributed_echo() -> Result<()> {
    let web_port = 7001_u32;
    let echo = Actor::from_file("./tests/modules/echo.wasm").unwrap();
    let actor_id = echo.public_key();
    let aid = actor_id.clone();

    let nc = nats::asynk::connect("0.0.0.0:4222").await?;
    let host_a = HostBuilder::new()
        .with_rpc_client(nc)
        .with_namespace("distributedecho")
        .build();

    host_a.start().await.unwrap();
    let nc2 = nats::asynk::connect("0.0.0.0:4222").await?;
    let host_b = HostBuilder::new()
        .with_rpc_client(nc2)
        .with_namespace("distributedecho")
        .build();

    host_b.start().await.unwrap();

    // ** NOTE ** - we need both hosts to be running before we start
    // so that host b will receive the claims from host a

    host_a.start_actor(echo).await.unwrap();
    await_actor_count(&host_a, 1, Duration::from_millis(50), 3)
        .await
        .unwrap();

    let arc = par_from_file("./tests/modules/libwascc_httpsrv.par.gz").unwrap();
    let httpserv = wascc_httpsrv::HttpServerProvider::new();
    let websrv = NativeCapability::from_instance(httpserv, None, arc.claims().unwrap()).unwrap();

    host_b.start_native_capability(websrv).await.unwrap();
    // always have to remember that "extras" is in the provider list.
    await_provider_count(&host_b, 2, Duration::from_millis(50), 3)
        .await
        .unwrap();

    let mut webvalues: HashMap<String, String> = HashMap::new();
    webvalues.insert("PORT".to_string(), format!("{}", web_port));
    host_b
        .set_link(
            &aid,
            "wascc:http_server",
            None,
            arc.claims().unwrap().subject.to_string(),
            webvalues,
        )
        .await
        .unwrap();

    delay_for(Duration::from_secs(1)).await;

    let url = format!("http://localhost:{}/foo/bar", web_port);
    let resp = reqwest::get(&url).await?;
    assert!(resp.status().is_success());
    assert_eq!(resp.text().await?,
     "{\"method\":\"GET\",\"path\":\"/foo/bar\",\"query_string\":\"\",\"headers\":{\"accept\":\"*/*\",\"host\":\"localhost:7001\"},\"body\":[]}");

    host_a.stop().await;
    host_b.stop().await;
    Ok(())
}

// Identical to the previous sample, except that a third (Started but empty) host
// is used to receive the set_link call, ensuring that any link can be set from
// anywhere in the lattice.
pub(crate) async fn link_on_third_host() -> Result<()> {
    const NS: &str = "linkonthirdhost";

    let nc = nats::asynk::connect("0.0.0.0:4222").await?;
    let host_a = HostBuilder::new()
        .with_rpc_client(nc)
        .with_namespace(NS)
        .build();

    host_a.start().await?;

    let nc2 = nats::asynk::connect("0.0.0.0:4222").await?;
    let host_b = HostBuilder::new()
        .with_rpc_client(nc2)
        .with_namespace(NS)
        .build();

    host_b.start().await?;

    let echo = Actor::from_file("./tests/modules/echo.wasm")?;
    let actor_id = echo.public_key();
    host_a.start_actor(echo).await?;
    await_actor_count(&host_a, 1, Duration::from_millis(50), 3).await?;

    let web_port = 7002_u32;
    let arc = par_from_file("./tests/modules/libwascc_httpsrv.par.gz")?;
    let websrv = NativeCapability::from_archive(&arc, None)?;

    host_b.start_native_capability(websrv).await?;
    // always have to remember that "extras" is in the provider list.
    await_provider_count(&host_b, 2, Duration::from_millis(50), 3).await?;

    let nc3 = nats::asynk::connect("0.0.0.0:4222").await?;
    let host_c = HostBuilder::new()
        .with_rpc_client(nc3)
        .with_namespace(NS)
        .build();

    host_c.start().await?;
    let mut webvalues: HashMap<String, String> = HashMap::new();
    webvalues.insert("PORT".to_string(), format!("{}", web_port));
    host_c
        .set_link(
            &actor_id,
            "wascc:http_server",
            None,
            arc.claims().unwrap().subject.to_string(),
            webvalues,
        )
        .await?;

    delay_for(Duration::from_millis(100)).await; // let the HTTP server spin up

    let url = format!("http://localhost:{}/foo/bar", web_port);
    let resp = reqwest::get(&url).await?;
    assert!(resp.status().is_success());
    assert_eq!(resp.text().await?,
               "{\"method\":\"GET\",\"path\":\"/foo/bar\",\"query_string\":\"\",\"headers\":{\"accept\":\"*/*\",\"host\":\"localhost:7002\"},\"body\":[]}");

    host_a.stop().await;
    host_b.stop().await;
    host_c.stop().await;
    Ok(())
}

// Run the kvcounter scenario, but with 1 instance of a HTTP provider, 2 instances
// of redis provider,  and 3 instances of the actor in a 5-host lattice.
// We can't do 2 instances of the HTTP provider because it would try and bind the same HTTP port twice
pub(crate) async fn scaled_kvcounter() -> Result<()> {
    use redis::Commands;
    let a = Actor::from_file("./tests/modules/kvcounter.wasm")?;
    let a_id = a.public_key();
    let websrv = par_from_file("./tests/modules/libwascc_httpsrv.par.gz")?;
    let web_id = websrv.claims().as_ref().unwrap().subject.to_string();
    let redis = par_from_file("./tests/modules/libwascc_redis.par.gz")?;
    let redis_id = redis.claims().as_ref().unwrap().subject.to_string();

    let host_a = scaledkv_host(Some(a), None).await?;
    let host_b = scaledkv_host(
        Some(Actor::from_file("./tests/modules/kvcounter.wasm")?),
        None,
    )
    .await?;
    let host_c = scaledkv_host(
        Some(Actor::from_file("./tests/modules/kvcounter.wasm")?),
        Some(vec![redis]),
    )
    .await?;
    let host_d = scaledkv_host(
        None,
        Some(vec![
            websrv,
            par_from_file("./tests/modules/libwascc_redis.par.gz")?,
        ]),
    )
    .await?;
    let host_e = scaledkv_host(
        None,
        Some(vec![par_from_file(
            "./tests/modules/libwascc_redis.par.gz",
        )?]),
    )
    .await?;

    let web_port = 6001_u32;

    let mut webvalues: HashMap<String, String> = HashMap::new();
    webvalues.insert("PORT".to_string(), format!("{}", web_port));

    let mut redisvalues: HashMap<String, String> = HashMap::new();
    redisvalues.insert("URL".to_string(), "redis://127.0.0.1:6379".to_string());

    host_a
        .set_link(
            &a_id,
            "wascc:http_server",
            None,
            web_id.to_string(),
            webvalues,
        )
        .await?;
    host_a
        .set_link(
            &a_id,
            "wascc:keyvalue",
            None,
            redis_id.to_string(),
            redisvalues,
        )
        .await?;

    let key = uuid::Uuid::new_v4().to_string();
    let rkey = format!(":{}", key); // the kv wasm logic does a replace on '/' with ':'
    let url = format!("http://localhost:{}/{}", web_port, key);

    let resp = reqwest::get(&url).await?;
    assert!(resp.status().is_success());
    let resp2 = reqwest::get(&url).await?;
    assert_eq!(resp2.text().await?, "{\"counter\":2}");

    let client = redis::Client::open("redis://127.0.0.1/")?;
    let mut con = client.get_connection()?;
    let _: () = con.del(&rkey)?;

    host_a.stop().await;
    host_b.stop().await;
    host_c.stop().await;
    host_d.stop().await;
    host_e.stop().await;

    Ok(())
}

async fn scaledkv_host(actor: Option<Actor>, par: Option<Vec<ProviderArchive>>) -> Result<Host> {
    const NS: &str = "scaledkvhost";
    let nc = nats::asynk::connect("0.0.0.0:4222").await?;

    let h = HostBuilder::new()
        .with_rpc_client(nc)
        .with_namespace(NS)
        .build();

    h.start().await?;
    if let Some(a) = actor {
        h.start_actor(a).await?;
        await_actor_count(&h, 1, Duration::from_millis(30), 3).await?;
    }
    if let Some(ref vp) = par {
        for p in vp {
            let nc = NativeCapability::from_archive(p, None)?;
            h.start_native_capability(nc).await?;
        }
        await_provider_count(&h, 1 + vp.len(), Duration::from_millis(30), 3).await?;
    }

    Ok(h)
}
