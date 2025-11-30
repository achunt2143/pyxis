cmake_minimum_required(VERSION 3.20)
project(GeckoOS-Gecko VERSION 1.0.0 LANGUAGES CXX)

# ============================================================================
# PROJECT SETTINGS
# ============================================================================

set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_CXX_FLAGS "${CMAKE_CXX_FLAGS} -Wall -Wextra -Wpedantic")

# Set build type
if(NOT CMAKE_BUILD_TYPE)
    set(CMAKE_BUILD_TYPE Release)
endif()

message(STATUS "Build type: ${CMAKE_BUILD_TYPE}")

# ============================================================================
# DEPENDENCIES
# ============================================================================

# Find required packages
find_package(PkgConfig REQUIRED)

# DBus
pkg_check_modules(DBUS REQUIRED dbus-1)
include_directories(${DBUS_INCLUDE_DIRS})
link_directories(${DBUS_LIBRARY_DIRS})

# libwebsockets
pkg_check_modules(LIBWEBSOCKETS REQUIRED libwebsockets)
include_directories(${LIBWEBSOCKETS_INCLUDE_DIRS})
link_directories(${LIBWEBSOCKETS_LIBRARY_DIRS})

# OpenGL
find_package(OpenGL REQUIRED)

# Boost
find_package(Boost 1.70 REQUIRED COMPONENTS system)

# spdlog (header-only)
find_package(spdlog REQUIRED)

# ============================================================================
# SOURCE FILES
# ============================================================================

set(GECKO_SOURCES
    # Headers
    gecko.h
    js-bridge.h
    rendering-context.h
    ipc-bridge.h
    websocket-server.h
    gecko-config.h
    
    # Implementation
    gecko-wrapper.cpp
    js-bridge.cpp
    rendering-context.cpp
    ipc-bridge.cpp
    websocket-server.cpp
)

# ============================================================================
# LIBRARY TARGET
# ============================================================================

add_library(gecko SHARED ${GECKO_SOURCES})

# ============================================================================
# INCLUDE DIRECTORIES
# ============================================================================

target_include_directories(gecko
    PUBLIC
        $<BUILD_INTERFACE:${CMAKE_CURRENT_SOURCE_DIR}>
        $<INSTALL_INTERFACE:include>
    PRIVATE
        ${CMAKE_CURRENT_SOURCE_DIR}
)

# ============================================================================
# LINK LIBRARIES
# ============================================================================

target_link_libraries(gecko
    PUBLIC
        ${OPENGL_LIBRARIES}
        Boost::system
        spdlog::spdlog
    PRIVATE
        ${DBUS_LIBRARIES}
        ${LIBWEBSOCKETS_LIBRARIES}
)

# ============================================================================
# COMPILER FLAGS
# ============================================================================

if(CMAKE_BUILD_TYPE STREQUAL "Release")
    target_compile_options(gecko PRIVATE
        -O3
        -DNDEBUG
    )
else()
    target_compile_options(gecko PRIVATE
        -g
        -O0
        -DDEBUG
    )
endif()

# ============================================================================
# INSTALLATION
# ============================================================================

# Install library
install(TARGETS gecko
    LIBRARY DESTINATION lib
    ARCHIVE DESTINATION lib
    RUNTIME DESTINATION bin
)

# Install headers
install(FILES
    gecko.h
    js-bridge.h
    rendering-context.h
    ipc-bridge.h
    websocket-server.h
    gecko-config.h
    DESTINATION include/geckoOS/gecko
)

# Install pkg-config file
configure_file(gecko.pc.in gecko.pc @ONLY)
install(FILES ${CMAKE_CURRENT_BINARY_DIR}/gecko.pc
    DESTINATION lib/pkgconfig
)

# Install configuration file
install(FILES defaults.conf
    DESTINATION etc/geckoOS
)

# ============================================================================
# EXECUTABLE (Optional test binary)
# ============================================================================

option(BUILD_GECKO_TEST "Build Gecko test executable" ON)

if(BUILD_GECKO_TEST)
    add_executable(gecko-test gecko-test.cpp)
    target_link_libraries(gecko-test gecko)
    
    install(TARGETS gecko-test
        RUNTIME DESTINATION bin
    )
endif()

# ============================================================================
# SUMMARY
# ============================================================================

message(STATUS "=== Gecko Integration Module ===")
message(STATUS "Version: ${PROJECT_VERSION}")
message(STATUS "Build type: ${CMAKE_BUILD_TYPE}")
message(STATUS "C++ standard: ${CMAKE_CXX_STANDARD}")
message(STATUS "")
message(STATUS "Dependencies:")
message(STATUS "  - DBus: ${DBUS_LIBRARIES}")
message(STATUS "  - libwebsockets: ${LIBWEBSOCKETS_LIBRARIES}")
message(STATUS "  - OpenGL: ${OPENGL_LIBRARIES}")
message(STATUS "  - Boost: ${Boost_LIBRARIES}")
message(STATUS "")
message(STATUS "Install prefix: ${CMAKE_INSTALL_PREFIX}")
