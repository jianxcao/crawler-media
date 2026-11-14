"""Confidence summaries using Media as the independent sampling unit."""
import random

from interval_metrics import percentile


def zero_failure_upper_bound(n, alpha=0.05):
    if type(n) is not int or n <= 0 or not 0 < alpha < 1:
        raise ValueError("invalid confidence parameters")
    return 1 - alpha ** (1 / n)


def cluster_bootstrap(rows, statistic, samples=2000, seed=20261103, alpha=0.05):
    """Resample whole Media groups, retaining every episode within each draw.

    Undefined statistics are omitted from quantiles and counted explicitly;
    they never turn into a perfect result. Percentile intervals are descriptive,
    not a substitute for representative independent Media groups.
    """
    if type(samples) is not int or samples <= 0 or not 0 < alpha < 1:
        raise ValueError("invalid bootstrap parameters")
    groups = {}
    for row in rows:
        groups.setdefault(row["media_group"], []).append(row)
    names = sorted(groups)
    rng = random.Random(seed)
    values = []
    if names:
        for _ in range(samples):
            sample = [row for _ in names for row in groups[rng.choice(names)]]
            value = statistic(sample)
            if value is not None:
                values.append(value)
    return {"lower": percentile(values, alpha / 2), "upper": percentile(values, 1 - alpha / 2),
            "groups": len(names), "cases": len(rows), "samples": samples,
            "defined_samples": len(values), "seed": seed, "alpha": alpha,
            "unit": "media_group", "method": "cluster_percentile_bootstrap"}
