# Delivery behavior

## Acknowledgment window

Consumers have 30 seconds from delivery to return an acknowledgement. Atlas Relay redelivers the event when that window expires without an acknowledgement.

Each redelivery starts a new 30-second window.
