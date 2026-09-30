#include <arpa/inet.h>
#include <array>
#include <asm-generic/socket.h>
#include <cassert>
#include <cerrno>
#include <cstddef>
#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <netinet/in.h>
#include <netinet/ip.h>
#include <print>
#include <string_view>
#include <sys/socket.h>
#include <sys/types.h>
#include <unistd.h>

[[noreturn]] static void die(const char *m){
    std::println(stderr, "[{}] {}", errno, m);
    abort();
}

constexpr size_t k_max_msg = 4096;

// read() may return FEWER bytes than asked in the loop until we have all the n.
static int32_t read_full(int fd, char *buf, size_t n){
    while (n > 0){
        ssize_t rv = read(fd, buf, n);
        if (rv <= 0) return -1;
        assert(static_cast<size_t>(rv) <= n);
        n -= static_cast<size_t>(rv);
        buf += rv;
    }
    return  0;
}

// write() may also be partial.
static int32_t write_all(int fd, const char *buf, size_t n){
    while(n > 0){
        ssize_t rv = write(fd, buf, n);
        if (rv <= 0) return -1;
        assert(static_cast<size_t>(rv) <= n);
        n -= static_cast<size_t>(rv);
        buf += rv;
    }
    return 0;
}

static int32_t one_request(int connfd){
    std::array<char, 4 + k_max_msg> rbuf;
    errno = 0;
    if (int32_t err = read_full(connfd, rbuf.data(), 4)){
        std::println(stderr, "{}", errno == 0 ? "EOF" : "read() error");
        return err;
    }
    uint32_t len = 0;
    memcpy(&len, rbuf.data(), 4);
    if (len > k_max_msg){
        std::println(stderr, "too long");
        return -1;
    }
    if (int32_t err = read_full(connfd, &rbuf[4], len)){
        std::println(stderr, "read() error");
        return err; 
    }
    std::println(stderr, "client says: {}", std::string_view(&rbuf[4], len));

    constexpr std::string_view reply = "world";
    std::array<char, 4 + reply.size()> wbuf;
    len = static_cast<uint32_t>(reply.size());
    memcpy(wbuf.data(), &len, 4);
    memcpy(&wbuf[4], reply.data(), len);
    return write_all(connfd, wbuf.data(), 4 + len);
}

static void do_something(int connfd){
    char rbuf[64] = {};
    ssize_t n = read(connfd, rbuf, sizeof(rbuf) - 1);
    if(n < 0){
        std::println(stderr, "read() error");
        return;
    }
    std::println(stderr, "client says: {}", rbuf);
    char wbuf[] = "world";
    write(connfd, wbuf, strlen(wbuf));
}

int main(){
    int fd = socket(AF_INET, SOCK_STREAM, 0);   // Creates a TCP Socket
    if (fd < 0) die("socket()");

    int val = 1;    // Allow quick restart on the same port
    setsockopt(fd, SOL_SOCKET, SO_REUSEADDR, &val, sizeof(val));

    sockaddr_in addr = {};
    addr.sin_family = AF_INET;
    addr.sin_port = htons(1234);
    addr.sin_addr.s_addr = htonl(0);
    if (bind(fd, reinterpret_cast<const sockaddr *>(&addr), sizeof(addr))) die("bind()");
    if(listen(fd, SOMAXCONN)) die("listen()");

    while (true){
        sockaddr_in client_addr= {};
        socklen_t addrlen = sizeof(client_addr);
        int connfd = accept(fd, reinterpret_cast<sockaddr *>(&client_addr), &addrlen);
        if (connfd < 0) continue;
        // serve one client until it disconnects as others must wait.
        while (one_request(connfd) == 0 ) {}
        close(connfd);    
    }
}