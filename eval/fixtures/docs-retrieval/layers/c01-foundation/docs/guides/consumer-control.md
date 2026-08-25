# Consumer control

## Pausing and resuming consumers

To pause a consumer, run `atlas consumers pause <consumer-id>`. The consumer finishes its in-flight delivery and accepts no new deliveries.

To resume a consumer, run `atlas consumers resume <consumer-id>`. Delivery continues from the consumer's existing cursor.
