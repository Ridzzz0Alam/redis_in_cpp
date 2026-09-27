#include <arpa/inet.h>
#include <cerrno>
#include <cstdlib>
#include <cstring>
#include <netinet/ip.h>
#include <print>
#include <sys/socket.h>
#include <unistd.h>

[[noreturn]] static void die(const char *m){
    std::println(stderr, "[{}] {}", errno, m);
    abort();
}