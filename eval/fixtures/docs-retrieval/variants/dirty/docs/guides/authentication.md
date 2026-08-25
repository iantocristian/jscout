# Authentication

## Relay v2 requests

Relay v2 publishers authenticate each HTTP request with the `Authorization` header. Set its value to `Relay <workspace-token>`.

The `X-Atlas-Key` header is a Relay v1 credential and must not be sent to the Relay v2 publishing endpoint.

For a delegated local test, put the session token in the `Atlas-Act-As` header alongside the normal `Authorization` header. A delegated session token never replaces the workspace token.
