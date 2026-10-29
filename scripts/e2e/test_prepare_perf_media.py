import importlib.util
from pathlib import Path
import sys
import tempfile
import unittest
import xml.etree.ElementTree as ET

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
spec = importlib.util.spec_from_file_location('prepare_perf_media', HERE / 'prepare-perf-media.py')
media = importlib.util.module_from_spec(spec)
spec.loader.exec_module(media)


class PerfMediaTests(unittest.TestCase):
    def test_local_fixture_has_valid_video_and_finite_delivery_sidecars(self):
        with tempfile.TemporaryDirectory(dir='/tmp') as root:
            video = media.prepare_tree(Path(root))
            self.assertGreater(video.stat().st_size, 1000)
            tracks = media.inspect_video(video)
            self.assertTrue(any(t['codec_type'] == 'video' for t in tracks['streams']))
            self.assertTrue(any(t['codec_type'] == 'audio' for t in tracks['streams']))
            self.assertTrue(tracks['chapters'])
            self.assertIn('Perfgate local fixture', video.with_suffix('.nfo').read_text())
            self.assertIn('00:00:00,000 --> 00:00:00,900', video.with_suffix('.en.srt').read_text())
            self.assertTrue(video.with_name(video.stem + '.fonts').is_dir())
            self.assertTrue(list(video.with_name(video.stem + '.fonts').glob('*.ttf')))
            self.assertTrue(video.with_name(video.stem + '.trickplay.jpg').read_bytes().startswith(b'\xff\xd8'))
            self.assertTrue(video.with_name(video.stem + '-chapter_0.jpg').read_bytes().startswith(b'\xff\xd8'))
            self.assertEqual(media.prepare_tree(Path(root)), video)

    def test_subtitle_resource_uses_advertised_track_index(self):
        session = {'decision': {'subtitles': [{'index': 1000, 'codec': 'subrip', 'is_external': True}]}}
        resources = media.subtitle_resources('file-id', 'work-id', session)
        self.assertEqual(resources, {'subtitle_file': 'file-id', 'subtitle_item': 'work-id',
                                     'subtitle_index': 1000, 'subtitle_codec': 'vtt'})

    def test_missing_track_is_not_fabricated(self):
        with self.assertRaises(ValueError):
            media.subtitle_resources('file', 'work', {'decision': {'subtitles': []}})

    def test_local_fixture_preserves_catalog_person_and_avatar_in_nfo(self):
        with tempfile.TemporaryDirectory(dir='/tmp') as root:
            video = media.prepare_tree(Path(root), {'name': 'Person & Name',
                                      'avatar_url': 'https://image.tmdb.org/a.jpg'})
            actor = ET.fromstring(video.with_suffix('.nfo').read_text()).find('actor')
            self.assertEqual(actor.findtext('name'), 'Person & Name')
            self.assertEqual(actor.findtext('thumb'), 'https://image.tmdb.org/a.jpg')
