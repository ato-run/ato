#!/bin/sh
set -eu

output=${1:-.tmp/pki}
mkdir -p "$output"

openssl genpkey -algorithm RSA -pkeyopt rsa_keygen_bits:3072 -out "$output/ca.key"
openssl req -x509 -new -sha256 -days 3650 -key "$output/ca.key" \
  -subj "/CN=Ato mail staging fixture CA" \
  -addext "basicConstraints=critical,CA:TRUE,pathlen:0" \
  -addext "keyUsage=critical,keyCertSign,cRLSign" \
  -addext "subjectKeyIdentifier=hash" -out "$output/fixture-ca.pem"

issue() {
  name=$1
  subject_alt_names=$2
  openssl genpkey -algorithm RSA -pkeyopt rsa_keygen_bits:2048 -out "$output/$name.key"
  openssl req -new -sha256 -key "$output/$name.key" -subj "/CN=$name.ato-mail.test" \
    -addext "subjectAltName=$subject_alt_names" -out "$output/$name.csr"
  openssl x509 -req -sha256 -days 365 -in "$output/$name.csr" \
    -CA "$output/fixture-ca.pem" -CAkey "$output/ca.key" -CAcreateserial \
    -copy_extensions copy -out "$output/$name.pem"
}

issue relay "DNS:relay.ato-mail.test,DNS:oci-linux-test.tail934987.ts.net"
issue mail "DNS:mail.ato-mail.test"
chmod 0600 "$output"/*.key
printf '%s\n' "$output"
