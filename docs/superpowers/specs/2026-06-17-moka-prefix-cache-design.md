# Moka Prefix Cache Design

## Goal

Replace the simulated Anthropic prompt-cache backing store with a bounded in-memory cache so stale prefix keys are evicted predictably and cache memory cannot grow without limit.

## Current Behavior

`src/anthropic/cache_sim.rs` stores simulated prompt-cache prefixes in a global `HashMap<u64, Instant>` guarded by `parking_lot::Mutex`. Each key is a cumulative prefix hash. The value is the last write time. On each cache-enabled request that reaches the cache lookup path, expired entries are removed with `retain`.

This approximates Anthropic's ephemeral prompt cache but has operational weaknesses:

- Expired entries remain in memory while traffic is idle.
- Requests without cache breakpoints can bypass cleanup.
- There is no max-capacity guard for high-cardinality prefixes.
- One mutex protects the whole map.

## Proposed Design

Use `moka::sync::Cache<u64, ()>` as the process-local backing cache.

Production cache configuration:

```rust
const CACHE_TTL: Duration = Duration::from_secs(5 * 60);
const MAX_CACHE_ENTRIES: u64 = 100_000;

static PREFIX_CACHE: LazyLock<Cache<u64, ()>> = LazyLock::new(|| {
    Cache::builder()
        .time_to_live(CACHE_TTL)
        .max_capacity(MAX_CACHE_ENTRIES)
        .build()
});
```

The simulated cache algorithm remains the same:

1. Traverse request content in Anthropic prefix order: `tools -> system -> messages`.
2. Record a cumulative prefix hash at every cache-control breakpoint.
3. Treat a fresh cache hit as `cache_read_input_tokens`.
4. Treat a cacheable but missed prefix as `cache_creation_input_tokens`.
5. Keep all post-breakpoint tail content in normal `input_tokens`.
6. Preserve `input + cache_creation + cache_read == total_input_tokens`.

Only the backing store changes:

- Hit check: `PREFIX_CACHE.get(&hash).is_some()`.
- Write/refresh: `PREFIX_CACHE.insert(hash, ())`.
- Manual `retain` cleanup is removed.
- No request content is stored in memory; only `u64` hashes are stored.

## Capacity Policy

`MAX_CACHE_ENTRIES` limits the number of cached prefix breakpoints, not the number of users or requests. A single request may write multiple entries if it contains multiple valid cache-control breakpoints.

When the cache exceeds capacity, `moka` may evict entries before their 5-minute TTL. This is acceptable for simulated billing because the conservative failure mode is a cache miss, causing the next matching request to be counted as cache creation rather than cache read.

## Testability

Introduce a small wrapper around the cache backend so tests can use a local cache with short TTL and small capacity instead of mutating the production global cache.

Suggested structure:

```rust
struct PrefixCache {
    cache: Cache<u64, ()>,
}

impl PrefixCache {
    fn new(ttl: Duration, max_capacity: u64) -> Self;
    fn contains(&self, hash: u64) -> bool;
    fn insert(&self, hash: u64);
}
```

`compute_split` keeps its public signature and delegates to an internal helper that accepts `&PrefixCache`. Tests call the helper with a local cache instance.

Required tests:

- First matching request creates cache entries and returns `cache_creation_input_tokens`.
- Second matching request within TTL returns `cache_read_input_tokens`.
- Requests in a different scope do not hit.
- Low-token prefixes below the model threshold do not write cache entries.
- A short-TTL test shows expired entries no longer hit.
- A low-capacity test shows evicted entries do not hit.
- A request without cache-control breakpoints does not write cache entries.

## Deployment Impact

This change adds a Rust compile-time dependency only:

```toml
moka = { version = "0.12", features = ["sync"] }
```

It does not require Redis, Memcached, another Docker container, extra ports, or docker-compose network changes. Existing runtime startup remains unchanged. Deployments using the prebuilt image must pull or build an image containing the new binary before restarting the container.

## Out Of Scope

This design does not add 5-minute versus 1-hour cache usage breakdown. It also does not change request conversion, Kiro upstream behavior, sub2api integration, Docker startup commands, or the `simulate-cache` configuration flag.
