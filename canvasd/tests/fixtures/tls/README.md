# Test TLS certificates

`cdn.rs`'s handshake tests serve HTTPS for `localhost` with these. They secure
nothing: `ca.der` is a throwaway CA the tests alone trust, and `localhost.der`
(key `localhost.key.der`, PKCS#8) is a leaf it signed, valid until 2126.

To make a new set, in an empty folder:

```sh
openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:prime256v1 -nodes -days 36500 \
  -subj "/CN=canvas test CA" -keyout ca.key -out ca.pem \
  -addext "basicConstraints=critical,CA:TRUE" -addext "keyUsage=critical,keyCertSign"
openssl req -newkey ec -pkeyopt ec_paramgen_curve:prime256v1 -nodes -subj "/CN=localhost" \
  -keyout leaf.key -out leaf.csr
printf 'basicConstraints=critical,CA:FALSE\nsubjectAltName=DNS:localhost\nextendedKeyUsage=serverAuth\nkeyUsage=critical,digitalSignature\n' > ext
openssl x509 -req -in leaf.csr -CA ca.pem -CAkey ca.key -CAcreateserial -days 36500 \
  -extfile ext -out leaf.pem
openssl x509 -in ca.pem -outform der -out ca.der
openssl x509 -in leaf.pem -outform der -out localhost.der
openssl pkcs8 -topk8 -nocrypt -in leaf.key -outform der -out localhost.key.der
```
