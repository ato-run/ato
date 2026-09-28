"""Coordinator-only fault proxy. Never intercepts the DeepSeek TLS connection."""
import http.client
import http.server
import json
import socket
import threading
import time
from .gates import before_claim
from .preflight import Stop, require

MAX_BODY=2*1024*1024


class CoordinatorProxy(http.server.BaseHTTPRequestHandler):
    protocol_version='HTTP/1.1'

    def log_message(self,*args):
        pass

    def body(self):
        if self.headers.get('transfer-encoding','').lower()=='chunked':
            result=bytearray()
            while True:
                line=self.rfile.readline(128)
                size=int(line.split(b';')[0],16)
                require(size>=0 and len(result)+size<=MAX_BODY,'proxy request bounds')
                if size==0:
                    require(self.rfile.readline(128)==b'\r\n','unsupported trailer')
                    return bytes(result)
                part=self.rfile.read(size)
                require(len(part)==size and self.rfile.read(2)==b'\r\n','truncated request')
                result.extend(part)
        size=int(self.headers.get('content-length','0'))
        require(0<=size<=MAX_BODY,'proxy request bounds')
        raw=self.rfile.read(size)
        require(len(raw)==size,'truncated request')
        return raw

    def upstream(self,method,path,body):
        # Fixed literal loopback target, no Location following or model endpoint.
        headers={'Content-Length':str(len(body))}
        for name in ('Authorization','Content-Type'):
            value=self.headers.get(name)
            if value:
                headers[name]=value
        conn=http.client.HTTPConnection('127.0.0.1',self.server.upstream_port,timeout=30)
        try:
            conn.request(method,path,body,headers)
            response=conn.getresponse()
            raw=response.read(16*1024*1024+1)
            require(len(raw)<=16*1024*1024,'Coordinator response bounds')
            return response.status,raw
        finally:
            conn.close()

    def handle_request(self):
        try:
            require(self.path.startswith('/v1/runtime-network/'),'unexpected Coordinator path')
            body=self.body()
            claim=self.path.endswith('/proposal/claim')
            complete=self.path.endswith('/proposal/complete')
            with self.server.lock:
                require(not self.server.stop_error,'run stopped')
                if claim:
                    require(not self.server.claim_seen and not self.server.recovery_only,'claim retry forbidden')
                    self.server.claim_seen=True
                    cached=self.server.last_status
                    evidence=before_claim(self.server.cell,cached,json.loads(body))
                    self.server.before_claim(cached,evidence)
                if complete:
                    require(not self.server.completion_seen and not self.server.recovery_only,'completion replay forbidden')
                    self.server.completion_seen=True
                    self.server.provider_ok = self.server.before_complete(self.server.claim_window) is not False
            status,data=self.upstream(self.command,self.path,body)
            if self.command=='GET' and status==200:
                value=json.loads(data)
                if isinstance(value,dict) and 'search_state' in value:
                    with self.server.lock:
                        self.server.last_status=value
                        if self.server.recovery_only:
                            self.server.recovery_gets+=1
            if complete and status==200 and self.server.drop_completion and self.server.provider_ok:
                # Confirm durable commit independently before dropping exactly
                # this Coordinator response; never drop a provider response.
                code,saved=self.upstream('GET',self.path.removesuffix('/proposal/complete'),b'')
                durable=json.loads(saved)
                require(code==200 and durable['proposal_round']['status'] in ('completed','invalid_output'),'completion not durably committed')
                self.server.saved_round=durable['proposal_round']
                self.server.recovery_only=True
                self.server.dropped.set()
                self.connection.shutdown(socket.SHUT_RDWR)
                self.connection.close()
                self.close_connection=True
                return
            if claim and status == 200:
                self.server.claim_window = {
                    'expires_at_ms': cached['proposal_round']['expires_at_ms'],
                    'claim_delivery_not_before_ms': int(time.time() * 1000),
                }
            self.send_response(status)
            self.send_header('Content-Type','application/json')
            self.send_header('Content-Length',str(len(data)))
            self.end_headers()
            self.wfile.write(data)
        except Exception:
            # Never retain exception text containing request bytes or headers.
            self.server.stop_error='coordinator_proxy_protocol_error'
            self.send_error(409,'execution stopped')

    do_GET=handle_request
    do_POST=handle_request


def create_proxy(cell,port,before,before_complete=lambda window: None):
    server=http.server.ThreadingHTTPServer(('127.0.0.1',0),CoordinatorProxy)
    server.upstream_port=port
    server.cell=cell
    server.before_claim=before
    server.before_complete=before_complete
    server.claim_window=None
    server.lock=threading.Lock()
    server.last_status=None
    server.claim_seen=False
    server.completion_seen=False
    server.recovery_only=False
    server.recovery_gets=0
    server.stop_error=None
    server.drop_completion=cell['id']=='G5'
    server.dropped=threading.Event()
    server.saved_round=None
    server.provider_ok=True
    threading.Thread(target=server.serve_forever,daemon=True).start()
    return server
