# Authentication

## Relay v1 requests

Relay v1 publishers authenticate each HTTP request with the `X-Atlas-Key` header. Set the header value to the workspace API key.

Do not put a Relay v1 API key in the `Authorization` header.
