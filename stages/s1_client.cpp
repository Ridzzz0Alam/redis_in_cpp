#include <arpa/inet.h>
#include <cerrno>
#include <cstdlib>
#include <cstring>
#include <netinet/in.h>
#include <netinet/ip.h>
#include <print>
#include <sys/socket.h>
#include <unistd.h>

[[noreturn]] static void die(const char *m) {
    std::println(stderr, "[{}] {}", errno, m);
    abort();
}

int main(){
    int fd = socket(AF_INET, SOCK_STREAM, 0);
    if (fd < 0) die("socket()");

    sockaddr_in addr = {};
    addr.sin_family = AF_INET;
    addr.sin_port = htons(1234);
    addr.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
    if (connect(fd, reinterpret_cast<const sockaddr *>(&addr), sizeof(addr))) die("connect");

    char msg[] = "hello";
    write(fd, msg, strlen(msg));

    char rbuf[64] = {};
    ssize_t n = read(fd, rbuf, sizeof(rbuf) - 1);
    if (n < 0) die("read");
    std::println("server says: {}", rbuf);
    close(fd);
}
