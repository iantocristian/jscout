# Webhook signatures

## Signature input

Atlas Relay signs the UTF-8 bytes of `<unix-seconds>.<raw-request-body>` with HMAC-SHA256. The request body is used exactly as delivered, before JSON parsing or whitespace normalization.

The hexadecimal digest is sent in the `Atlas-Signature` header.
