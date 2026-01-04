#!/usr/bin/env python3
"""
Unit tests for cleanup.py
"""

import unittest
from unittest.mock import patch, MagicMock, mock_open
import sys
import os

# Add parent directory to path to import cleanup
sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

import cleanup

class TestCleanup(unittest.TestCase):

    @patch('cleanup.run_command')
    def test_stop_services(self, mock_run):
        """Test service stopping."""
        mock_run.return_value = ('', '')
        cleanup.stop_services()
        self.assertTrue(mock_run.called)
        # Check that systemctl commands were called
        calls = [str(call) for call in mock_run.call_args_list]
        self.assertTrue(any('systemctl stop' in str(call) for call in calls))

    @patch('cleanup.input', return_value='DESTROY')
    def test_confirm_destruction_yes(self, mock_input):
        """Test destruction confirmation with yes."""
        cleanup.confirm_destruction()
        # Should not exit

    @patch('cleanup.input', return_value='no')
    def test_confirm_destruction_no(self, mock_input):
        """Test destruction confirmation with no."""
        with self.assertRaises(SystemExit):
            cleanup.confirm_destruction()

    @patch('cleanup.run_command')
    @patch('os.path.exists')
    def test_remove_files(self, mock_exists, mock_run):
        """Test file removal."""
        mock_exists.return_value = False
        mock_run.return_value = ('', '')
        cleanup.remove_files()
        # Should complete without errors

    @patch('cleanup.input', return_value='n')
    @patch('os.path.exists')
    def test_remove_rust_skip(self, mock_exists, mock_input):
        """Test Rust removal when user skips."""
        mock_exists.return_value = True
        cleanup.remove_rust()
        # Should skip without errors

    @patch('cleanup.input', return_value='y')
    @patch('shutil.rmtree')
    @patch('os.path.exists')
    def test_remove_rust_confirm(self, mock_exists, mock_rmtree, mock_input):
        """Test Rust removal when user confirms."""
        mock_exists.return_value = True
        cleanup.remove_rust()
        # Should attempt to remove directories

    @patch('cleanup.run_command')
    @patch('os.path.exists')
    def test_remove_caddy_repo(self, mock_exists, mock_run):
        """Test Caddy repository removal."""
        mock_exists.return_value = False
        mock_run.return_value = ('', '')
        cleanup.remove_caddy_repo()
        # Should complete without errors

    @patch('cleanup.input', return_value='n')
    @patch('cleanup.run_command')
    def test_reset_firewall_skip(self, mock_run, mock_input):
        """Test firewall reset when user skips."""
        mock_run.return_value = ('', '')
        cleanup.reset_firewall()
        # Should skip without errors

    @patch('cleanup.input', return_value='n')
    @patch('cleanup.run_command')
    def test_uninstall_packages_skip(self, mock_run, mock_input):
        """Test package uninstall when user skips."""
        mock_run.return_value = ('', '')
        cleanup.uninstall_packages()
        # Should skip without errors

if __name__ == '__main__':
    unittest.main()
