"""Interval arithmetic in the original source timeline (milliseconds)."""


def overlap_ms(left, right):
    return max(0, min(left[1], right[1]) - max(left[0], right[0]))


def merge_intervals(intervals):
    merged = []
    for start, end in sorted(intervals):
        if merged and start <= merged[-1][1]:
            merged[-1][1] = max(merged[-1][1], end)
        else:
            merged.append([start, end])
    return merged


def signed_distance(value, allowed):
    if value < allowed[0]:
        return value - allowed[0]
    if value > allowed[1]:
        return value - allowed[1]
    return 0


def iou(left, right):
    intersection = overlap_ms(left, right)
    return intersection / (left[1] - left[0] + right[1] - right[0] - intersection)


def percentile(values, quantile):
    if not values:
        return None
    values = sorted(values)
    position = (len(values) - 1) * quantile
    lower = int(position)
    upper = min(lower + 1, len(values) - 1)
    return values[lower] + (values[upper] - values[lower]) * (position - lower)


def distribution(values):
    return {"count": len(values), "p50": percentile(values, 0.5), "p95": percentile(values, 0.95)}
