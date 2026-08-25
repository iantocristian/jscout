# Emergency drain

## Drain command

Run `atlas consumers drain <consumer-id> --reject-new` to finish in-flight work without accepting new deliveries. The command exits when the in-flight count reaches zero.
