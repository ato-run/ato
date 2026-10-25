import http from 'node:http';
import {parse} from 'cookie';

// A small real HTTP workload for the infrastructure acceptance gates.
// No provider, receipt mock, source patch or production binding is involved.
http.createServer((request, response) => {
  response.writeHead(200, {'Content-Type': 'text/plain'});
  response.end(parse('probe=hello').probe + '\n');
}).listen(Number(process.env.ATO_ENDPOINT_APP_HTTP_PORT || 8080), '127.0.0.1');
