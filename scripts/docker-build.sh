#!/bin/bash
# ============================================================================
# GeckoOS Docker Build Helper Script
# Automates Docker image building with proper checks and tagging
# ============================================================================

set -euo pipefail

# Color codes for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

# Script configuration
PROJECT_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DOCKER_IMAGE_NAME="geckoOS"
DOCKER_IMAGE_TAG="${1:-latest}"
DOCKER_BUILDKIT=1

# ============================================================================
# LOGGING FUNCTIONS
# ============================================================================

log_info() {
    echo -e "${BLUE}[INFO]${NC} $*"
}

log_success() {
    echo -e "${GREEN}[✓]${NC} $*"
}

log_warn() {
    echo -e "${YELLOW}[WARN]${NC} $*"
}

log_error() {
    echo -e "${RED}[ERROR]${NC} $*" >&2
}

# ============================================================================
# UTILITY FUNCTIONS
# ============================================================================

check_docker_installed() {
    if ! command -v docker &> /dev/null; then
        log_error "Docker is not installed"
        log_info "Install Docker: https://docs.docker.com/get-docker/"
        exit 1
    fi
    log_success "Docker found: $(docker --version)"
}

check_docker_daemon() {
    if ! docker ps &> /dev/null; then
        log_error "Docker daemon is not running"
        log_info "Start Docker and try again"
        exit 1
    fi
    log_success "Docker daemon is running"
}

check_dockerfile() {
    if [ ! -f "${PROJECT_ROOT}/Dockerfile" ]; then
        log_error "Dockerfile not found at ${PROJECT_ROOT}/Dockerfile"
        exit 1
    fi
    log_success "Dockerfile found"
}

get_image_size() {
    local image="$1"
    docker images --format "table {{.Repository}}:{{.Tag}}\t{{.Size}}" | grep "${image}" | awk '{print $2}'
}

# ============================================================================
# BUILD FUNCTIONS
# ============================================================================

build_docker_image() {
    local image_name="$1"
    local tag="$2"
    local dockerfile="${PROJECT_ROOT}/Dockerfile"
    local context="${PROJECT_ROOT}"

    log_info "Starting Docker build..."
    log_info "Image: ${image_name}:${tag}"
    log_info "Context: ${context}"

    # Build with BuildKit for better caching
    DOCKER_BUILDKIT=1 docker build \
        --file "${dockerfile}" \
        --tag "${image_name}:${tag}" \
        --tag "${image_name}:latest" \
        --progress=plain \
        --build-arg BUILDKIT_INLINE_CACHE=1 \
        "${context}" 2>&1

    if [ $? -eq 0 ]; then
        log_success "Docker image built successfully"
        return 0
    else
        log_error "Docker build failed"
        return 1
    fi
}

tag_image() {
    local image_name="$1"
    local source_tag="$2"
    local target_tag="$3"

    log_info "Tagging image as ${image_name}:${target_tag}..."
    docker tag "${image_name}:${source_tag}" "${image_name}:${target_tag}"
    log_success "Image tagged successfully"
}

show_image_info() {
    local image_name="$1"
    local tag="$2"

    log_info "Image Information:"
    echo "  Name: ${image_name}:${tag}"
    echo "  Size: $(get_image_size "${image_name}:${tag}")"

    log_info "Image created: $(docker inspect "${image_name}:${tag}" --format='{{.Created}}')"
    log_info "Image digest: $(docker inspect "${image_name}:${tag}" --format='{{.RepoDigests}}')"
}

# ============================================================================
# MAIN EXECUTION
# ============================================================================

main() {
    echo "============================================================================"
    echo "GeckoOS Docker Build Helper"
    echo "============================================================================"
    echo ""

    # Verify Docker installation
    log_info "Checking Docker installation..."
    check_docker_installed
    check_docker_daemon

    # Verify Dockerfile exists
    log_info "Checking project files..."
    check_dockerfile

    # Build image
    echo ""
    log_info "Building Docker image..."
    if build_docker_image "${DOCKER_IMAGE_NAME}" "${DOCKER_IMAGE_TAG}"; then
        # Tag additional versions
        tag_image "${DOCKER_IMAGE_NAME}" "${DOCKER_IMAGE_TAG}" "dev"

        # Show information
        echo ""
        show_image_info "${DOCKER_IMAGE_NAME}" "${DOCKER_IMAGE_TAG}"

        # List images
        echo ""
        log_info "Available GeckoOS images:"
        docker images | grep "${DOCKER_IMAGE_NAME}" | sed 's/^/  /'

        # Print usage instructions
        echo ""
        echo "============================================================================"
        echo "Build Complete! Usage Instructions:"
        echo "============================================================================"
        echo ""
        echo "Run interactive shell:"
        echo "  $ docker-compose run dev bash"
        echo ""
        echo "Run build in container:"
        echo "  $ docker-compose run build"
        echo ""
        echo "View logs:"
        echo "  $ docker-compose logs -f build"
        echo ""
        echo "Clean up:"
        echo "  $ docker-compose down -v"
        echo ""

        log_success "Docker build completed successfully"
        return 0
    else
        log_error "Docker build failed. See output above for details."
        return 1
    fi
}

# Run main function
main "$@"
