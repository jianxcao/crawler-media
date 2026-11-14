"""Explicit recommendation readiness and full-result observations, without bodies."""
from collections import Counter
import json
import math
import time


def similar_payload(payload, contract='stateful'):
    try:
        response = json.loads(payload)
    except (ValueError, TypeError):
        return None
    if not isinstance(response, dict) or response.get('ok') is not True:
        return None
    data = response.get('data')
    if contract == 'legacy-list':
        return {'state': 'ready', 'items': data} if isinstance(data, list) else None
    if contract != 'stateful':
        return None
    if not isinstance(data, dict):
        return None
    if data.get('state') == 'ready' and isinstance(data.get('items'), list):
        return data
    delay = data.get('retry_after_ms')
    if (data.get('state') == 'pending' and 'items' not in data
            and isinstance(delay, (int, float)) and not isinstance(delay, bool)
            and math.isfinite(delay) and delay > 0):
        return data
    return None


def similar_response(code, payload, contract='stateful'):
    data = similar_payload(payload, contract)
    if data and ((code == 200 and data['state'] == 'ready')
                 or (code == 202 and data['state'] == 'pending')):
        return data
    return None


def measure_similar(client, url, deadline_seconds=30, initial=None, started_at=None, contract='stateful'):
    started = time.perf_counter() if started_at is None else started_at
    deadline = started + deadline_seconds
    response = initial if initial is not None else client.raw(url)
    code, elapsed, payload = response
    data = similar_response(code, payload, contract)
    observation = {'initial_code': code, 'initial_ms': elapsed,
                   'initial_state': data['state'] if data else 'error',
                   'terminal_code': code, 'terminal_state': 'error',
                   'items_count': None, 'pending_responses': 0, 'ready_responses': 0,
                   'completion_ms': None, 'passed': False}
    while True:
        code, _, payload = response
        data = similar_response(code, payload, contract)
        observation['terminal_code'] = code
        if data is None:
            break
        if data['state'] == 'ready':
            observation.update(terminal_state='ready', items_count=len(data['items']),
                               ready_responses=1, passed=True)
            break
        observation['pending_responses'] += 1
        remaining = deadline - time.perf_counter()
        if remaining <= 0:
            observation.update(terminal_code=0, terminal_state='timeout')
            break
        time.sleep(min(max(data['retry_after_ms'] / 1000, .001), .5, remaining))
        if time.perf_counter() >= deadline:
            observation.update(terminal_code=0, terminal_state='timeout')
            break
        response = client.raw(url)
    observation['completion_ms'] = max(elapsed, (time.perf_counter() - started) * 1000)
    return observation


def summarize_similar(observations, percentile, contract='stateful'):
    durations = [item['completion_ms'] for item in observations]
    return {'contract': contract,
            'historical_latency_comparability': 'legacy initial response is a complete list; stateful initial response may be pending; completion remains explicit',
            'initial_pending': sum(item['initial_state'] == 'pending' for item in observations),
            'initial_ready': sum(item['initial_state'] == 'ready' for item in observations),
            'terminal_codes': dict(Counter(item['terminal_code'] for item in observations)),
            'completed_ready': sum(item['terminal_state'] == 'ready' for item in observations),
            'complete_body_p50_ms': percentile(durations, .50),
            'complete_body_p95_ms': percentile(durations, .95),
            'complete_body_p99_ms': percentile(durations, .99),
            'passed': all(item['passed'] for item in observations),
            'observations': observations}
