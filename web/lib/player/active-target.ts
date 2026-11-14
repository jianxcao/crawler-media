/**
 * The player's one active target: logical coordinates plus an optional *explicit*
 * physical source. Episode navigation replaces the whole target, so a source the user
 * picked for one episode never leaks into the next (R09). The default displayed file is
 * never an explicit choice (R10).
 */
export interface ActiveTarget {
  season?: number;
  episode?: number;
  fileId?: string;
}

export interface TargetUnit {
  media_item_id: string;
  season_number?: number;
  episode_number?: number;
  file_id?: string;
}

export function initialTarget(input: {
  season?: number;
  episode?: number;
  fileId?: string;
}): ActiveTarget {
  const coordinates =
    input.season !== undefined && input.episode !== undefined
      ? { season: input.season, episode: input.episode }
      : {};
  return input.fileId ? { ...coordinates, fileId: input.fileId } : coordinates;
}

/** Next/prev/autoplay: a fresh logical target without any physical preference. */
export function navigateTarget(
  _previous: ActiveTarget,
  to: { season: number; episode: number },
): ActiveTarget {
  return { season: to.season, episode: to.episode };
}

export function sessionUnit(mediaItemId: string, target: ActiveTarget): TargetUnit {
  const unit: TargetUnit = { media_item_id: mediaItemId };
  if (target.season !== undefined && target.episode !== undefined) {
    unit.season_number = target.season;
    unit.episode_number = target.episode;
  }
  if (target.fileId) unit.file_id = target.fileId;
  return unit;
}

/** Only a real user selection becomes a play link's `file_id`. */
export function playLinkFileId(selectedTrackFileId: string | null | undefined): string | undefined {
  return selectedTrackFileId ?? undefined;
}
