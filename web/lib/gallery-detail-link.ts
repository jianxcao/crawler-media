export function galleryDetailHref(
  group: { library_id: string; media_item_id: string },
  image: { season?: number | null; episode?: number | null },
): string {
  const base = `/library/${group.library_id}/item/${group.media_item_id}`;
  const hasSeason = typeof image.season === "number" && !isNaN(image.season);
  const hasEpisode = typeof image.episode === "number" && !isNaN(image.episode);
  if (hasSeason && hasEpisode) {
    return `${base}?season=${image.season}&episode=${image.episode}`;
  }
  return base;
}
