# Default target
default:
    just --list

# Recipe to build the project
build:
    cargo build --release

# Recipe to run the development server (assuming 'dx serve' is a wrapper around running the built binary)
serve:
    dx serve

# Helper recipe to ensure dependencies are installed
deps:
    cargo build

echo TEST:
    echo {{TEST}}