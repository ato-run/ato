#!/usr/bin/env python3
"""V2 controller integration test: loopback synthetic model, real local Runtime.

Never a live acceptance result. Overrides only the requester transport mode to
new_mock and the final output filename. Production execute has no mock switch.
"""
import argparse
import http.server
import json
import pathlib
import sys
import threading
from unittest.mock import patch
sys.dont_write_bytecode = True
from execution import preflight, runner


class Model(http.server.BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def do_POST(self):
        raw = self.rfile.read(int(self.headers['Content-Length']))
        body = json.loads(raw)
        request = json.loads(body['messages'][1]['content'])
        cell = next(c for c in self.server.plan['cells'] if c['search_id'] == request['search_id'])
        self.server.calls.append((cell['id'], raw))
        if cell['id'] == 'G3':
            proposals = [{'kind':'propose_derivation','operations':[{'operation':'shell','argv':['escape']}]}]
        elif cell['id'] == 'G4':
            proposals = [{'kind':'unsupported'}]
        else:
            proposals = [{'kind':'propose_derivation','operations':[{'operation':'python_http_process@1','entrypoint_id':'e_42'}]}]
        if cell['id'] == 'G2':
            assert request['failure_evidence'], 'G2 must come from an actual failed attempt'
        content = json.dumps({'schema':'ato.formation-proposal/1','proposals':proposals})
        data = json.dumps({'model':'deepseek-flash','choices':[{'finish_reason':'stop','index':0,
            'message':{'role':'assistant','content':content}}],
            'usage':{'prompt_tokens':321,'completion_tokens':123}}).encode()
        self.send_response(200)
        self.send_header('Content-Type','application/json')
        self.send_header('Content-Length',str(len(data)))
        self.end_headers()
        self.wfile.write(data)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ('ato','api','plan','binaries','run','journal'):
        parser.add_argument('--'+name,type=pathlib.Path,required=True)
    args = parser.parse_args()
    args.run.mkdir(parents=True,exist_ok=False)
    plan, configs, manifest, _ = preflight.check(args)
    server = http.server.ThreadingHTTPServer(('127.0.0.1',0),Model)
    server.plan = plan
    server.calls = []
    threading.Thread(target=server.serve_forever,daemon=True).start()
    original = runner.LocalRun.configure

    def configure(local,cell,config):
        folder, token, rid, config = original(local,cell,config)
        mock = config.pop('live_llm')
        mock['config']['endpoint'] = f'http://127.0.0.1:{server.server_port}'
        config['mock_llm'] = mock
        return folder,token,rid,config

    def save(args,plan,results,stopped=False,partial_cell=None):
        evidence = {'mode':'synthetic_loopback_controller_integration','results':results,
                    'stopped':stopped,'partial_cell':partial_cell,'model_http_calls':len(server.calls),
                    'live_calls':0,'key_read':False}
        preflight.write_new(args.run/'mock-controller-results.json',evidence)
        if not stopped:
            assert [cell for cell,_ in server.calls] == plan['cell_order']
            assert all(result['gate']=='PASS' for result in results)
            for result,(_,raw) in zip(results,server.calls):
                assert result['provider_body_sha256']=='sha256:'+preflight.digest(raw)
                request = json.loads(raw)['messages'][1]['content'].encode()
                assert result['proposal_request_sha256']=='sha256:'+preflight.digest(request)
            print('controller actual mock G0-G5 PASS; synthetic sends=6; live calls=0; key unread')
    try:
        preflight.initialize_budget(args,manifest)  # Synthetic run, never a live journal.
        with patch.object(runner.LocalRun,'configure',configure),patch.object(runner,'persist_results',save):
            runner.execute(args,plan,configs,manifest)
    finally:
        server.shutdown();server.server_close()


if __name__ == '__main__':
    main()
