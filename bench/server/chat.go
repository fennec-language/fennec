// The twin of `chat.fen`: a hub goroutine owns the rooms, each connection has a goroutine reading
// it and one writing to it, and they talk over channels of the same sizes as `chat.fen`'s.
package main

import (
	"bufio"
	"fmt"
	"net"
	"strings"
)

// What the hub hears from the connections: a join carries the member's mailbox, a message its
// line, and a leave neither.
type note struct {
	name    string
	room    string
	line    string
	mailbox chan string
	leaving bool
}

// Writes each line the hub sends one connection, until the hub lets go of its mailbox.
func deliver(inbox chan string, connection net.Conn) {
	for line := range inbox {
		if _, err := connection.Write([]byte(line)); err != nil {
			// Drained, so the hub is never held up by a member that has gone.
			for range inbox {
			}
			return
		}
	}
}

// The hub: each room's members, by name, and each message sent on to all of them.
func hub(inbox chan note) {
	rooms := map[string]map[string]chan string{}
	for n := range inbox {
		switch {
		case n.mailbox != nil:
			if rooms[n.room] == nil {
				rooms[n.room] = map[string]chan string{}
			}
			rooms[n.room][n.name] = n.mailbox
			n.mailbox <- "JOINED " + n.room + "\n"
		case n.leaving:
			if mailbox, ok := rooms[n.room][n.name]; ok {
				close(mailbox)
				delete(rooms[n.room], n.name)
			}
		default:
			for _, member := range rooms[n.room] {
				member <- n.line
			}
		}
	}
}

// One connection: it joins its room, then hands the hub each line it sends, until it hangs up.
func attend(connection net.Conn, toHub chan note) {
	defer connection.Close()
	lines := bufio.NewScanner(connection)
	if !lines.Scan() {
		return
	}
	parts := strings.Split(lines.Text(), " ")
	if len(parts) != 3 || parts[0] != "JOIN" {
		return
	}
	room, name := parts[1], parts[2]
	mailbox := make(chan string, 64)
	done := make(chan struct{})
	go func() {
		deliver(mailbox, connection)
		close(done)
	}()
	toHub <- note{name: name, room: room, mailbox: mailbox}
	for lines.Scan() {
		toHub <- note{room: room, line: name + " " + lines.Text() + "\n"}
	}
	toHub <- note{name: name, room: room, leaving: true}
	<-done
}

func main() {
	server, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		panic(err)
	}
	fmt.Println(server.Addr().(*net.TCPAddr).Port)
	toHub := make(chan note, 1024)
	go hub(toHub)
	for {
		connection, err := server.Accept()
		if err != nil {
			continue
		}
		go attend(connection, toHub)
	}
}
