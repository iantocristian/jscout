# Retry policy

## Rate-limit responses

When Atlas Relay returns **`429 Too Many Requests`**, wait for the **`Retry-After`** duration before sending another publish request.

Retry delays are:

- taken from `Retry-After`;
- applied once per request;
- reset after a successful publish.

## Retry budget

A producer may spend at most 90 seconds retrying one publish request. Stop retrying when that total budget is exhausted, even when the latest `Retry-After` duration extends past it.
