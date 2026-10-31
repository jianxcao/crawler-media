/** Independent read failures for the two requests used by FavoritesView. */
export interface FavoriteFailures { wall: boolean; gallery: boolean; }
export interface FavoriteReadOutcome { channel: keyof FavoriteFailures; failed: boolean; }
export const initialFavoriteFailures: FavoriteFailures = { wall: false, gallery: false };

export function favoriteFailures(
  state: FavoriteFailures,
  outcome: FavoriteReadOutcome,
): FavoriteFailures {
  return { ...state, [outcome.channel]: outcome.failed };
}
