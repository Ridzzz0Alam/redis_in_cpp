#include <arpa/inet.h>
#include <asm-generic/socket.h>
#include <cerrno>
#include <cstdlib>
#include <cstring>
#include <netinet/in.h>
#include <netinet/ip.h>
#include <print>
#include <sys/socket.h>
#include <unistd.h>

[[noreturn]] static void die(const char *m){
    std::println(stderr, "[{}] {}", errno, m);
    abort();
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
        do_something(connfd);
        close(connfd);    
    }




}