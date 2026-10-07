#!/usr/bin/env python3
"""Reject unowned/forged acquisition boundaries without changing clock bounds."""
import copy
import importlib.util
from pathlib import Path
import unittest

ROOT=Path(__file__).resolve().parents[2]
spec=importlib.util.spec_from_file_location('drained_decoder',ROOT/'scripts/lib/gnome_sysprof.py')
decoder=importlib.util.module_from_spec(spec);spec.loader.exec_module(decoder)


class Boundary(unittest.TestCase):
    def fixture(self):
        def mark(name,stamp,message):
            return {'pid':42,'monotonic_ns':stamp,'duration_ns':0,'name':name,'message':message}
        rows=[mark('Roost::CaptureBoundary',20_000_000,
                   'output=Virtual-1 clock=0xabcd next=5 pending=0 depth=0 dispatched=0 state=scheduled requested_us=10000 enabled_us=20000 samples=2'),
              mark('Clutter::FrameClock::dispatch()',21_000_000,'Virtual-1'),
              mark('Roost::FrameClock::dispatch-id',21_000_100,'output=Virtual-1 frame=5 dispatch_us=21000'),
              mark('Clutter::FrameClock::presented()',22_000_000,'Virtual-1'),
              mark('Roost::FrameClock::presented-id',22_000_100,'output=Virtual-1 view_frame=5 global_frame=100 presentation_us=21900 sequence=22 flags=5 kms_ready_us=21500'),
              mark('Roost::KMS::raw-page-flip',22_000_200,'crtc=39 sequence=22 seconds=0 microseconds=21900 device=/dev/dri/card1')]
        return {'marks':rows},{'pid':42,'drained_start_required':True,'start_requested_monotonic_ns':10_000_000,'started_monotonic_ns':20_500_000}
    def test_real_boundary_source_counter_and_first_dispatch_accepted(self):
        d,m=self.fixture();r=decoder.capture_boundary_evidence(d,m)
        self.assertEqual(r['next_counter'],5);self.assertEqual(r['first_source_dispatch']['frame_counter'],5)
    def test_missing_duplicate_or_foreign_owner_boundary_rejected(self):
        for mutation in ('missing','duplicate','foreign'):
            d,m=self.fixture()
            if mutation=='missing':d['marks'].pop(0)
            elif mutation=='duplicate':d['marks'].append(copy.deepcopy(d['marks'][0]))
            else:d['marks'][0]['pid']=43
            with self.subTest(mutation=mutation),self.assertRaises(ValueError):decoder.capture_boundary_evidence(d,m)
    def test_busy_slots_depth_dispatched_and_wrong_output_rejected(self):
        for before,after in [('pending=0','pending=1'),('depth=0','depth=1'),('dispatched=0','dispatched=1'),
                             ('state=scheduled','state=dispatched-one'),('Virtual-1','Virtual-2'),('clock=0xabcd','clock=0x0')]:
            d,m=self.fixture();d['marks'][0]['message']=d['marks'][0]['message'].replace(before,after)
            with self.subTest(after=after),self.assertRaises(ValueError):decoder.capture_boundary_evidence(d,m)
    def test_actual_wait_deadline_sample_bound_and_clock_order_rejected(self):
        for before,after in [('samples=2','samples=0'),('samples=2','samples=5001'),
                             ('requested_us=10000','requested_us=20001'),('enabled_us=20000','enabled_us=20001'),
                             ('enabled_us=20000','enabled_us=5010000')]:
            d,m=self.fixture();d['marks'][0]['message']=d['marks'][0]['message'].replace(before,after)
            with self.subTest(after=after),self.assertRaises(ValueError):decoder.capture_boundary_evidence(d,m)
    def test_boundary_after_acknowledgement_or_before_request_rejected(self):
        for key,value in [('started_monotonic_ns',19_000_000),('start_requested_monotonic_ns',21_000_000),('drained_start_required',False)]:
            d,m=self.fixture();m[key]=value
            with self.subTest(key=key),self.assertRaises(ValueError):decoder.capture_boundary_evidence(d,m)
    def test_original_leading_unowned_completion_is_not_trimmed(self):
        d,m=self.fixture();bad=dict(d['marks'][3],monotonic_ns=20_100_000);d['marks'].insert(1,bad)
        with self.assertRaises(ValueError):decoder.capture_boundary_evidence(d,m)
        self.assertIn(bad,d['marks'])
    def test_wrong_next_source_counter_missing_source_or_preboundary_dispatch_rejected(self):
        for mutation in ('counter','source','early'):
            d,m=self.fixture()
            if mutation=='counter':d['marks'][2]['message']=d['marks'][2]['message'].replace('frame=5','frame=6')
            elif mutation=='source':d['marks'].pop(2)
            else:d['marks'][1]['monotonic_ns']=19_000_000
            with self.subTest(mutation=mutation),self.assertRaises(ValueError):decoder.capture_boundary_evidence(d,m)
    def test_boundary_does_not_replace_strict_owned_frame_clock_checks(self):
        d,m=self.fixture();decoder.capture_boundary_evidence(d,m)
        with self.assertRaises(ValueError):decoder.overview_frame_bounds(d,m['pid'])

if __name__=='__main__':unittest.main()
