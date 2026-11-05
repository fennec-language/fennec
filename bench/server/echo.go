// The twin of `echo.fen`: each connection a goroutine.
package main

import (
	"bufio"
	"fmt"
	"net"
)

func answer(connection net.Conn) {
	defer connection.Close()
	lines := bufio.NewScanner(connection)
	for lines.Scan() {
		if _, err := connection.Write(append(lines.Bytes(), '\n')); err != nil {
			return
		}
	}
}

func main() {
	server, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		panic(err)
	}
	fmt.Println(server.Addr().(*net.TCPAddr).Port)
	for {
		connection, err := server.Accept()
		if err != nil {
			panic(err)
		}
		go answer(connection)
	}
}
