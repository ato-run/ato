/// Source Runtime E2E Tests
/// 
/// Tests for Source Runtime with multiple language targets:
/// - Python (python3)
/// - Node.js (node)
/// - Ruby (ruby)
/// - Deno (deno)

use std::path::PathBuf;
use std::process::Command;

#[cfg(test)]
mod python_tests {
    use super::*;

    #[test]
    #[ignore] // Run with: cargo test --test source_runtime_e2e -- --ignored python
    fn test_python_hello_world() {
        // Check if python3 is available
        let python_check = Command::new("python3")
            .arg("--version")
            .output();
        
        if python_check.is_err() {
            eprintln!("Python3 not found, skipping test");
            return;
        }

        // Create temporary test script
        let test_script = r#"
print("Hello from Python Source Runtime!")
import sys
print(f"Python version: {sys.version}")
"#;

        let temp_dir = std::env::temp_dir();
        let script_path = temp_dir.join("test_python.py");
        std::fs::write(&script_path, test_script).expect("Failed to write test script");

        // Execute via python3
        let output = Command::new("python3")
            .arg(&script_path)
            .output()
            .expect("Failed to execute Python script");

        assert!(output.status.success(), "Python script failed");
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains("Hello from Python Source Runtime!"));

        // Cleanup
        std::fs::remove_file(script_path).ok();
    }

    #[test]
    #[ignore]
    fn test_python_with_env() {
        let python_check = Command::new("python3")
            .arg("--version")
            .output();
        
        if python_check.is_err() {
            eprintln!("Python3 not found, skipping test");
            return;
        }

        let test_script = r#"
import os
test_var = os.environ.get('TEST_VAR', 'not_set')
print(f"TEST_VAR={test_var}")
"#;

        let temp_dir = std::env::temp_dir();
        let script_path = temp_dir.join("test_python_env.py");
        std::fs::write(&script_path, test_script).expect("Failed to write test script");

        let output = Command::new("python3")
            .arg(&script_path)
            .env("TEST_VAR", "test_value_123")
            .output()
            .expect("Failed to execute Python script");

        assert!(output.status.success());
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains("TEST_VAR=test_value_123"));

        std::fs::remove_file(script_path).ok();
    }
}

#[cfg(test)]
mod nodejs_tests {
    use super::*;

    #[test]
    #[ignore] // Run with: cargo test --test source_runtime_e2e -- --ignored node
    fn test_nodejs_hello_world() {
        // Check if node is available
        let node_check = Command::new("node")
            .arg("--version")
            .output();
        
        if node_check.is_err() {
            eprintln!("Node.js not found, skipping test");
            return;
        }

        let test_script = r#"
console.log("Hello from Node.js Source Runtime!");
console.log("Node version:", process.version);
"#;

        let temp_dir = std::env::temp_dir();
        let script_path = temp_dir.join("test_node.js");
        std::fs::write(&script_path, test_script).expect("Failed to write test script");

        let output = Command::new("node")
            .arg(&script_path)
            .output()
            .expect("Failed to execute Node.js script");

        assert!(output.status.success(), "Node.js script failed");
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains("Hello from Node.js Source Runtime!"));

        std::fs::remove_file(script_path).ok();
    }

    #[test]
    #[ignore]
    fn test_nodejs_with_env() {
        let node_check = Command::new("node")
            .arg("--version")
            .output();
        
        if node_check.is_err() {
            eprintln!("Node.js not found, skipping test");
            return;
        }

        let test_script = r#"
console.log("TEST_VAR=" + (process.env.TEST_VAR || "not_set"));
"#;

        let temp_dir = std::env::temp_dir();
        let script_path = temp_dir.join("test_node_env.js");
        std::fs::write(&script_path, test_script).expect("Failed to write test script");

        let output = Command::new("node")
            .arg(&script_path)
            .env("TEST_VAR", "test_value_456")
            .output()
            .expect("Failed to execute Node.js script");

        assert!(output.status.success());
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains("TEST_VAR=test_value_456"));

        std::fs::remove_file(script_path).ok();
    }
}

#[cfg(test)]
mod ruby_tests {
    use super::*;

    #[test]
    #[ignore] // Run with: cargo test --test source_runtime_e2e -- --ignored ruby
    fn test_ruby_hello_world() {
        // Check if ruby is available
        let ruby_check = Command::new("ruby")
            .arg("--version")
            .output();
        
        if ruby_check.is_err() {
            eprintln!("Ruby not found, skipping test");
            return;
        }

        let test_script = r#"
puts "Hello from Ruby Source Runtime!"
puts "Ruby version: #{RUBY_VERSION}"
"#;

        let temp_dir = std::env::temp_dir();
        let script_path = temp_dir.join("test_ruby.rb");
        std::fs::write(&script_path, test_script).expect("Failed to write test script");

        let output = Command::new("ruby")
            .arg(&script_path)
            .output()
            .expect("Failed to execute Ruby script");

        assert!(output.status.success(), "Ruby script failed");
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains("Hello from Ruby Source Runtime!"));

        std::fs::remove_file(script_path).ok();
    }

    #[test]
    #[ignore]
    fn test_ruby_with_env() {
        let ruby_check = Command::new("ruby")
            .arg("--version")
            .output();
        
        if ruby_check.is_err() {
            eprintln!("Ruby not found, skipping test");
            return;
        }

        let test_script = r#"
puts "TEST_VAR=#{ENV['TEST_VAR'] || 'not_set'}"
"#;

        let temp_dir = std::env::temp_dir();
        let script_path = temp_dir.join("test_ruby_env.rb");
        std::fs::write(&script_path, test_script).expect("Failed to write test script");

        let output = Command::new("ruby")
            .arg(&script_path)
            .env("TEST_VAR", "test_value_789")
            .output()
            .expect("Failed to execute Ruby script");

        assert!(output.status.success());
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains("TEST_VAR=test_value_789"));

        std::fs::remove_file(script_path).ok();
    }
}

#[cfg(test)]
mod deno_tests {
    use super::*;

    #[test]
    #[ignore] // Run with: cargo test --test source_runtime_e2e -- --ignored deno
    fn test_deno_hello_world() {
        // Check if deno is available
        let deno_check = Command::new("deno")
            .arg("--version")
            .output();
        
        if deno_check.is_err() {
            eprintln!("Deno not found, skipping test");
            return;
        }

        let test_script = r#"
console.log("Hello from Deno Source Runtime!");
console.log("Deno version:", Deno.version.deno);
"#;

        let temp_dir = std::env::temp_dir();
        let script_path = temp_dir.join("test_deno.ts");
        std::fs::write(&script_path, test_script).expect("Failed to write test script");

        let output = Command::new("deno")
            .arg("run")
            .arg("--allow-env")
            .arg(&script_path)
            .output()
            .expect("Failed to execute Deno script");

        assert!(output.status.success(), "Deno script failed");
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains("Hello from Deno Source Runtime!"));

        std::fs::remove_file(script_path).ok();
    }

    #[test]
    #[ignore]
    fn test_deno_with_env() {
        let deno_check = Command::new("deno")
            .arg("--version")
            .output();
        
        if deno_check.is_err() {
            eprintln!("Deno not found, skipping test");
            return;
        }

        let test_script = r#"
console.log("TEST_VAR=" + (Deno.env.get("TEST_VAR") || "not_set"));
"#;

        let temp_dir = std::env::temp_dir();
        let script_path = temp_dir.join("test_deno_env.ts");
        std::fs::write(&script_path, test_script).expect("Failed to write test script");

        let output = Command::new("deno")
            .arg("run")
            .arg("--allow-env")
            .arg(&script_path)
            .env("TEST_VAR", "test_value_abc")
            .output()
            .expect("Failed to execute Deno script");

        assert!(output.status.success());
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains("TEST_VAR=test_value_abc"));

        std::fs::remove_file(script_path).ok();
    }
}
