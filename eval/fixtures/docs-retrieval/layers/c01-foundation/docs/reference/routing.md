# Routing specification

## Partition assignment

Atlas Relay encodes the event's `route_key` as UTF-8, computes its 64-bit FNV-1a hash, and assigns the event to `hash % partition_count`.

The assignment rule remains stable for the lifetime of a stream. Changing the partition count creates a new stream generation.

## Missing route keys

An event without a route key uses its event identifier as the partition input.
