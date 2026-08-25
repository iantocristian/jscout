# Atlas Relay changelog

## 2025-02-20

- Added a console panel for recent route-key and partition-assignment activity. The panel does not change the routing protocol.
- Added `Retry-After` telemetry for `429 Too Many Requests` responses. Retry behavior did not change.
- Renamed the authentication-header metric emitted by Relay v2 publishers. Authentication behavior did not change.

## 2024-01-15

- Published the Relay v1 HTTP interface.
- Added consumer pause and resume controls.
- Documented route-key partition assignment.
