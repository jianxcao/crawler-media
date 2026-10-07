export interface NavigableEpisode {
  season_number?: number | null;
  episode_number: number;
  owned?: boolean;
  name?: string | null;
}

export function nextOwnedEpisode<T extends NavigableEpisode>(
  episodes: T[],
  currentSeason: number,
  currentEpisode: number,
): T | null {
  const candidate = episodes
    .filter((item) => {
      const seasonMatches =
        item.season_number === undefined ||
        item.season_number === null ||
        item.season_number === currentSeason;
      return seasonMatches && item.episode_number > currentEpisode && (item.owned ?? true);
    })
    .sort((a, b) => a.episode_number - b.episode_number)[0];
  return candidate ?? null;
}

export function prevOwnedEpisode<T extends NavigableEpisode>(
  episodes: T[],
  currentSeason: number,
  currentEpisode: number,
): T | null {
  const candidate = episodes
    .filter((item) => {
      const seasonMatches =
        item.season_number === undefined ||
        item.season_number === null ||
        item.season_number === currentSeason;
      return seasonMatches && item.episode_number < currentEpisode && (item.owned ?? true);
    })
    .sort((a, b) => b.episode_number - a.episode_number)[0];
  return candidate ?? null;
}
