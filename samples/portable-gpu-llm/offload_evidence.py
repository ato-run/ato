"""Placement evidence from the pinned b11429 logger, never from launch options."""
import json
import re

ENGINE_REVISION = 'd81235049384534c167caea52b85a694f6103d14'
MODEL_LAYERS = 28  # qwen2.block_count in the digest-pinned Qwen GGUF
LINE_LIMIT = 16384


class OffloadEvidence:
    def __init__(self):
        self.pending = {'stdout': b'', 'stderr': b''}
        self.dropping = {'stdout': False, 'stderr': False}
        self.model_layers = None
        self.placements = {}
        self.summary = None
        self.invalid = None
        self.buffer_fallback = None
        self.discarded_records = 0

    def feed(self, stream, chunk):
        # Parse before bounded tail truncation, keeping only matched facts.
        parts = chunk.split(b'\n')
        for index, part in enumerate(parts):
            complete = index < len(parts) - 1
            if not self.dropping[stream]:
                if len(self.pending[stream]) + len(part) > LINE_LIMIT:
                    self.pending[stream] = b''
                    self.dropping[stream] = True
                    self.discarded_records += 1
                else:
                    self.pending[stream] += part
            if complete:
                if not self.dropping[stream]:
                    self.record(self.pending[stream])
                self.pending[stream] = b''
                self.dropping[stream] = False

    def record(self, raw):
        try:
            record = json.loads(raw)
        except (ValueError, UnicodeError):
            return
        if (not isinstance(record, dict) or record.get('type') != 'log'
                or not isinstance(record.get('msg'), str)):
            return
        message = record['msg'].rstrip('\n')
        if record.get('level') == 'info':
            total = re.fullmatch(r'print_info: n_layer_all\s*= (\d+)', message)
            summary = re.fullmatch(r'load_tensors: offloaded (\d+)/(\d+) layers to GPU', message)
            if total:
                value = int(total[1])
                if value != MODEL_LAYERS or self.model_layers not in (None, value):
                    self.invalid = 'model_layer_count_mismatch'
                self.model_layers = value
            if summary:
                value = [int(summary[1]), int(summary[2])]
                if value[1] != MODEL_LAYERS + 1 or not 0 <= value[0] <= value[1] or self.summary not in (None, value):
                    self.invalid = 'offload_summary_conflict'
                self.summary = value
        if record.get('level') == 'debug':
            if message.startswith('done_getting_tensors: tensor ') and 'cannot be used with preferred buffer type ' in message:
                # b11429 reports only the first fallback and a count of others;
                # it cannot prove that the others exclude offloadable layers.
                self.buffer_fallback = message[:512]
            if message.startswith('tensor ') and ' buffer type overridden to ' in message:
                self.invalid = 'tensor_buffer_override'
            layer = re.fullmatch(r'load_tensors: layer\s+(\d+) assigned to device (\S+), is_swa = [01]', message)
            if layer:
                index, device = int(layer[1]), layer[2]
                if index > MODEL_LAYERS or len(device) > 32:
                    self.invalid = 'layer_assignment_out_of_range'
                    return
                if index in self.placements and self.placements[index] != device:
                    self.invalid = 'layer_assignment_conflict'
                self.placements[index] = device

    def snapshot(self):
        total = MODEL_LAYERS + 1
        complete = self.model_layers == MODEL_LAYERS and set(self.placements) == set(range(total))
        gpu = sum(device == 'CUDA0' for device in self.placements.values())
        state = 'missing'
        invalid_reason = self.invalid
        if self.invalid:
            state = 'invalid'
        elif complete:
            if gpu < total:
                state = 'insufficient'
            elif self.buffer_fallback:
                state = 'invalid'
                invalid_reason = 'tensor_buffer_fallback'
            elif self.summary == [gpu, total]:
                state = 'full'
            elif self.summary is not None:
                state = 'invalid'
                invalid_reason = 'placement_summary_mismatch'
        return {'schema': 'ato.llama-layer-placement/1', 'engine_revision': ENGINE_REVISION,
                'source': 'b11429_jsonl_model_metadata_and_layer_device_assignments',
                'state': state, 'model_layers': self.model_layers, 'target_total_layers': total,
                'observed_layer_assignments': len(self.placements), 'observed_gpu_layer_assignments': gpu,
                'gpu_placed_layers': gpu if state in ('full', 'insufficient') else None,
                'placements': [{'layer': i, 'device': device} for i, device in sorted(self.placements.items())],
                'reported_offload_summary': self.summary, 'invalid_reason': invalid_reason,
                'buffer_fallback': self.buffer_fallback,
                'discarded_oversized_records': self.discarded_records}
