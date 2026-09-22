/*
 * pipebridge.c — tiny named-pipe <-> TCP pump for ZaloCall.exe under Wine.
 *
 * Build: i686-w64-mingw32-gcc pipebridge.c -lws2_32 -o pipebridge.exe
 * Run (inside Wine): pipebridge.exe <tcpRecvPort> <tcpSendPort> [authToken]
 *
 * Creates the named pipes ZaloCall.exe connects to:
 *   \\.\pipe\PipeZCallRecv  (helper -> main)   -> TCP 127.0.0.1:<tcpRecvPort>
 *   \\.\pipe\PipeZCallSend  (main -> helper)   -> TCP 127.0.0.1:<tcpSendPort>
 *
 * Auth: when [authToken] is given, the token + '\n' is sent as the first
 * line of every TCP connection; the main app drops connections that do not
 * start with the token.
 */
#include <winsock2.h>
#include <windows.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

typedef struct {
    HANDLE pipe;
    SOCKET sock;
    int dir; /* 0 = pipe->sock, 1 = sock->pipe */
} PumpArgs;

static const char *g_token = NULL; /* NULL = no auth */

static DWORD WINAPI pump(LPVOID arg) {
    PumpArgs *a = (PumpArgs *)arg;
    char buf[65536];
    DWORD got;
    int sent;

    if (a->dir == 0) {
        /* pipe -> socket */
        while (ReadFile(a->pipe, buf, sizeof(buf), &got, NULL) && got > 0) {
            sent = send(a->sock, buf, (int)got, 0);
            if (sent <= 0) break;
        }
    } else {
        /* socket -> pipe */
        DWORD written;
        while ((sent = recv(a->sock, buf, sizeof(buf), 0)) > 0) {
            if (!WriteFile(a->pipe, buf, (DWORD)sent, &written, NULL)) break;
        }
    }
    return 0;
}

static void bridge(const char *pipeName, unsigned short tcpPort, const char *label) {
    HANDLE hPipe;
    SOCKET sock;
    struct sockaddr_in addr;
    PumpArgs a1, a2;
    HANDLE t1, t2;

    for (;;) {
        hPipe = CreateNamedPipeA(pipeName,
            PIPE_ACCESS_DUPLEX,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
            1, 65536, 65536, 0, NULL);
        if (hPipe == INVALID_HANDLE_VALUE) {
            fprintf(stderr, "[%s] CreateNamedPipe failed: %lu\n", label, GetLastError());
            return;
        }
        fprintf(stderr, "[%s] pipe listening: %s\n", label, pipeName);

        if (!ConnectNamedPipe(hPipe, NULL) &&
            GetLastError() != ERROR_PIPE_CONNECTED) {
            fprintf(stderr, "[%s] ConnectNamedPipe failed: %lu\n", label, GetLastError());
            CloseHandle(hPipe);
            Sleep(1000);
            continue;
        }
        fprintf(stderr, "[%s] helper connected\n", label);

        sock = socket(AF_INET, SOCK_STREAM, IPPROTO_TCP);
        if (sock == INVALID_SOCKET) {
            fprintf(stderr, "[%s] socket failed: %d\n", label, WSAGetLastError());
            CloseHandle(hPipe);
            Sleep(1000);
            continue;
        }
        memset(&addr, 0, sizeof(addr));
        addr.sin_family = AF_INET;
        addr.sin_port = htons(tcpPort);
        addr.sin_addr.s_addr = inet_addr("127.0.0.1");
        if (connect(sock, (struct sockaddr *)&addr, sizeof(addr)) != 0) {
            fprintf(stderr, "[%s] tcp connect failed: %d\n", label, WSAGetLastError());
            closesocket(sock);
            CloseHandle(hPipe);
            Sleep(1000);
            continue;
        }
        fprintf(stderr, "[%s] tcp connected: %u\n", label, tcpPort);

        if (g_token && g_token[0]) {
            char line[256];
            int n = snprintf(line, sizeof(line), "%s\n", g_token);
            if (n > 0 && send(sock, line, (int)n, 0) <= 0) {
                fprintf(stderr, "[%s] token send failed\n", label);
                closesocket(sock);
                DisconnectNamedPipe(hPipe);
                CloseHandle(hPipe);
                continue;
            }
        }

        a1.pipe = hPipe; a1.sock = sock; a1.dir = 0;
        a2.pipe = hPipe; a2.sock = sock; a2.dir = 1;
        t1 = CreateThread(NULL, 0, pump, &a1, 0, NULL);
        t2 = CreateThread(NULL, 0, pump, &a2, 0, NULL);
        WaitForSingleObject(t1, INFINITE);
        /* The pipe->socket pump exits when ZaloCall goes away. Unblock the
           socket->pipe pump (it may be parked in recv) so the loop can
           re-create the pipes and wait for the next ZaloCall instance. */
        shutdown(sock, SD_BOTH);
        WaitForSingleObject(t2, 5000);

        fprintf(stderr, "[%s] connection ended, waiting for reconnect\n", label);
        closesocket(sock);
        DisconnectNamedPipe(hPipe);
        CloseHandle(hPipe);
    }
}

typedef struct {
    const char *pipeName;
    unsigned short tcpPort;
    const char *label;
} BridgeArgs;

static DWORD WINAPI bridgeThread(LPVOID arg) {
    BridgeArgs *b = (BridgeArgs *)arg;
    bridge(b->pipeName, b->tcpPort, b->label);
    return 0;
}

int main(int argc, char **argv) {
    WSADATA wsa;

    /* Self-test used by the Linux app to verify this wine can run 32-bit
       executables: pipebridge.exe --version prints and exits. */
    if (argc > 1 && strcmp(argv[1], "--version") == 0) {
        printf("pipebridge 1\n");
        return 0;
    }

    unsigned short recvPort = (argc > 1) ? (unsigned short)atoi(argv[1]) : 29631;
    unsigned short sendPort = (argc > 2) ? (unsigned short)atoi(argv[2]) : 29632;

    WSAStartup(MAKEWORD(2, 2), &wsa);
    if (argc > 3 && argv[3][0]) g_token = argv[3];
    fprintf(stderr, "[pipebridge] recv=%u send=%u auth=%s\n", recvPort, sendPort,
            g_token ? "yes" : "no");

    BridgeArgs r = { "\\\\.\\pipe\\PipeZCallRecv", recvPort, "RECV" };
    BridgeArgs s = { "\\\\.\\pipe\\PipeZCallSend", sendPort, "SEND" };
    CreateThread(NULL, 0, bridgeThread, &r, 0, NULL);
    CreateThread(NULL, 0, bridgeThread, &s, 0, NULL);

    /* keep the process alive */
    for (;;) Sleep(60000);
    return 0;
}
