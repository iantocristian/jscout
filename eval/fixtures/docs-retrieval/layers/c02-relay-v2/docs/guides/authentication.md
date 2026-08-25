# Authentication

## Relay v2 requests

Relay v2 publishers authenticate each HTTP request with the `Authorization` header. Set its value to `Relay <workspace-token>`.

The `X-Atlas-Key` header is a Relay v1 credential and must not be sent to the Relay v2 publishing endpoint.
