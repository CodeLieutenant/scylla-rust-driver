# Load balancing

## Introduction

The driver uses a load balancing policy to determine which node(s) and shard(s)
to contact when executing a query. Load balancing policies implement the
`LoadBalancingPolicy` trait, which contains methods to generate a load
balancing plan based on the query information and the state of the cluster.

Load balancing policies do not influence to which nodes connections are
being opened. For a node connection blacklist configuration refer to
`scylla::policies::host_filter::HostFilter`, which can be set session-wide
using `SessionBuilder::host_filter` method.

In this chapter, "target" will refer to a pair `<node, optional shard>`.

## Plan

When a query is prepared to be sent to the database, the load balancing policy
constructs a load balancing plan. This plan is essentially a list of targets to
which the driver will try to send the query. The first elements of the plan are
the targets which are the best to contact (e.g. they might be replicas for the
requested data or have the best latency).

## Policy

The Scylla/Cassandra driver provides a default load balancing policy (see
[Default Policy](default-policy.md) for details), but you can
also implement your own custom policies that better suit your specific use
case. To use a custom policy, you simply need to implement the
`LoadBalancingPolicy` trait and pass an instance of your custom policy to the
used execution profile.

Our recommendation is to use [`Default Policy`](default-policy.md) with token-
awareness enabled and latency-awareness disabled.

## Configuration

Load balancing policies can be configured in three different ways (sorted by descending precedence):
1. directly on the `Statement`, `PreparedStatement` or `Batch` (`Statement::set_load_balancing_policy()`)
2. execution profile set on the statement (`Statement::set_execution_profile_handle()`)
3. default execution profile set on the session (`SessionBuilder::default_execution_profile_handle()`)

 In the code sample provided, a new execution profile is created using
`ExecutionProfile::builder()`, and the load balancing policy is set to the
`DefaultPolicy` using `.load_balancing_policy(policy)`.

The newly created execution profile is then converted to a handle using
`.into_handle()`, and passed as the default execution profile to the
`SessionBuilder` using `.default_execution_profile_handle(handle)`.

```rust
# extern crate scylla;
# use std::error::Error;
# async fn check_only_compiles(uri: &str) -> Result<(), Box<dyn Error>> {
use scylla::client::session::Session;
use scylla::client::session_builder::SessionBuilder;
use scylla::policies::load_balancing::DefaultPolicy;
use scylla::client::execution_profile::ExecutionProfile;
use std::sync::Arc;

let policy = Arc::new(DefaultPolicy::default());

let profile = ExecutionProfile::builder()
    .load_balancing_policy(policy)
    .build();
let handle = profile.into_handle();

let session: Session = SessionBuilder::new()
    .known_node(&uri)
    .default_execution_profile_handle(handle)
    .build()
    .await?;
# return Ok(())
# }
```

In addition to being able to configure load balancing policies through
execution profiles at the session level, the driver also allow for setting
execution profile handles on a per-query basis. This means that for each query,
a specific execution profile can be selected with a customized load balancing
settings.

## `LoadBalancingPolicy` trait

### `pick` and `fallback`:

Most queries are sent successfully on the first try. In such cases, only the
first element of the load balancing plan is needed, so it's usually unnecessary
to compute entire load balancing plan. To optimize this common case, the
`LoadBalancingPolicy` trait provides two methods: `pick` and `fallback`.

`pick` returns the first target to contact for a given query, which is usually
the best based on a particular load balancing policy.

`fallback`, returns an iterator that provides the rest of the targets in the
load balancing plan. `fallback` is called when using the initial picked
target fails (or when executing speculatively) or when `pick` returned `None`.

It's possible for the `fallback` method to include the same target that was
returned by the `pick` method. In such cases, the query execution layer filters
out the picked target from the iterator returned by `fallback`.

### `on_query_success` and `on_query_failure`:

The `on_query_success` and `on_query_failure` methods are useful for load
balancing policies because they provide feedback on the performance and health
of the nodes in the cluster.

When a query is successfully executed, the `on_query_success` method is called
and can be used by the load balancing policy to update its internal state. For
example, a policy might use the latency of the successful query to update its
latency statistics for each node in the cluster. This information can be used
to make decisions about which nodes to contact in the future.

On the other hand, when a query fails to execute, the `on_query_failure` method
is called and provides information about the failure. The error message
returned by Cassandra can help determine the cause of the failure, such as a
node being down or overloaded. The load balancing policy can use this
information to update its internal state and avoid contacting the same node
again until it's recovered.

```{eval-rst}
.. toctree::
   :hidden:
   :glob:

   default-policy
```
