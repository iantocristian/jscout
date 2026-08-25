# Retry policy

## Rate-limit responses

When Atlas Relay returns **`429 Too Many Requests`**, wait for the **`Retry-After`** duration before sending another publish request.

Retry delays are:

- taken from `Retry-After`;
- applied once per request;
- reset after a successful publish.
