#!/usr/bin/env python3
"""
Unit tests for deploy.py
"""

import unittest
from unittest.mock import patch, MagicMock, mock_open
import sys
import os

# Add parent directory to path to import deploy
sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

import deploy

class TestDeploy(unittest.TestCase):

    @patch('deploy.run_command')
    def test_install_build_tools(self, mock_run):
        """Test build tools installation."""
        deploy.install_build_tools()
        self.assertTrue(mock_run.called)
        # Check that apt update and install were called
        calls = [str(call) for call in mock_run.call_args_list]
        self.assertTrue(any('apt update' in str(call) for call in calls))
        self.assertTrue(any('apt install' in str(call) for call in calls))

    @patch('deploy.run_command')
    @patch('deploy.platform.machine')
    def test_detect_architecture(self, mock_machine, mock_run):
        """Test architecture detection."""
        mock_machine.return_value = 'aarch64'
        arch = deploy.detect_architecture()
        self.assertEqual(arch, 'aarch64')

    @patch('deploy.tempfile.TemporaryDirectory')
    @patch('deploy.requests.get')
    @patch('deploy.run_command')
    def test_install_youki(self, mock_run, mock_get, mock_tempdir):
        """Test youki installation."""
        # Mock temporary directory
        mock_tempdir.return_value.__enter__.return_value = '/tmp/test'
        
        # Mock download response
        mock_response = MagicMock()
        mock_response.iter_content.return_value = [b'fake tarball']
        mock_get.return_value = mock_response
        
        # Mock version check
        mock_run.return_value = ('youki 0.3.3', '')
        
        with patch('builtins.open', mock_open()):
            deploy.install_youki()
        
        mock_get.assert_called_once()
        self.assertTrue(mock_run.called)

    @patch('deploy.run_command')
    @patch('deploy.requests.get')
    @patch('deploy.tempfile.NamedTemporaryFile')
    @patch('os.path.exists')
    @patch('os.unlink')
    def test_install_rust(self, mock_unlink, mock_exists, mock_tempfile, mock_get, mock_run):
        """Test Rust installation."""
        # Mock that Rust is not installed
        mock_run.side_effect = [
            Exception("not found"),  # First check fails
            ('', ''),  # installer runs
            ('', ''),  # source cargo env
            ('rustc 1.70.0', '')  # version check succeeds
        ]
        
        mock_exists.return_value = True
        mock_file = MagicMock()
        mock_file.name = '/tmp/rustup.sh'
        mock_tempfile.return_value = mock_file
        
        mock_response = MagicMock()
        mock_response.text = '#!/bin/sh\necho "fake installer"'
        mock_get.return_value = mock_response
        
        deploy.install_rust()
        
        mock_get.assert_called_once()

    @patch('deploy.run_command')
    def test_install_caddy(self, mock_run):
        """Test Caddy installation."""
        # Mock that Caddy is not installed initially
        mock_run.side_effect = [
            Exception("not found"),  # First check fails
            ('', ''),  # GPG key add
            ('', ''),  # Repo add
            ('', ''),  # apt update
            ('', ''),  # apt install
            ('caddy v2.6.0', '')  # version check
        ]
        
        deploy.install_caddy()
        
        self.assertTrue(mock_run.called)

    @patch('deploy.run_command')
    def test_setup_firewall(self, mock_run):
        """Test firewall setup."""
        # Mock run_command to return proper tuples
        mock_run.return_value = ('Status: active', '')
        
        deploy.setup_firewall()
        
        # Check that UFW commands were called
        calls = [str(call) for call in mock_run.call_args_list]
        self.assertTrue(any('ufw' in str(call) for call in calls))
        self.assertTrue(any('80/tcp' in str(call) for call in calls))
        self.assertTrue(any('443/tcp' in str(call) for call in calls))

    @patch('builtins.open', new_callable=mock_open)
    @patch('deploy.tempfile.NamedTemporaryFile')
    @patch('deploy.requests.get')
    @patch('deploy.run_command')
    def test_download_binary(self, mock_run, mock_get, mock_temp, mock_file):
        """Test binary download."""
        mock_response = MagicMock()
        mock_response.iter_content.return_value = [b'fake binary']
        mock_get.return_value = mock_response
        
        mock_temp_file = MagicMock()
        mock_temp_file.name = '/tmp/binary'
        mock_temp.return_value = mock_temp_file
        
        deploy.download_binary()
        
        mock_get.assert_called_once()
        self.assertTrue(mock_run.called)

if __name__ == '__main__':
    unittest.main()
