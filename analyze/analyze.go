package main

import (
	"context"
	"encoding/binary"
	"errors"
	"flag"
	"fmt"
	"log"
	"os"
	"os/exec"
	"runtime"
	"sort"
	"strings"
	"time"

	"nhooyr.io/websocket"
)

const (
	eventTypeDisplay = 0x00
	eventTypeMouse   = 0x04
	wsAddr           = "ws://127.0.0.1:12345"
)

func main() {
	var sleep time.Duration
	var duration time.Duration
	var curve string

	flag.DurationVar(&sleep, "sleep", 1*time.Second, "Duration to sleep before connecting")
	flag.DurationVar(&duration, "duration", 10*time.Second, "Duration to collect data")
	flag.StringVar(&curve, "curve", "output", "Output filename prefix")
	flag.Parse()

	// Sleep first
	log.Printf("Sleeping for %v…", sleep)
	time.Sleep(sleep)

	// Connect to WebSocket
	ctx := context.Background()
	log.Printf("Connecting to WebSocket at %s…", wsAddr)
	conn, _, err := websocket.Dial(ctx, wsAddr, nil)
	if err != nil {
		log.Fatal("Failed to connect:", err)
	}
	defer conn.Close(websocket.StatusNormalClosure, "")

	log.Printf("Collecting mouse movement data for %v…", duration)
	intervals := collectMouseIntervals(ctx, conn, duration)
	if len(intervals) == 0 {
		log.Fatal("No mouse movement data collected")
	}

	log.Printf("Collected %d mouse movement intervals", len(intervals))

	// Dump the 10 longest delays
	dumpLongestDelays(intervals, 10)

	if err := generateVisualization(curve, intervals); err != nil {
		log.Fatalf("Failed to generate visualization: %v", err)
	}
}

func collectMouseIntervals(ctx context.Context, conn *websocket.Conn, duration time.Duration) []uint64 {
	var intervals []uint64
	ctx, cancel := context.WithTimeout(ctx, duration)
	defer cancel()

	for {
		readCtx, readCancel := context.WithTimeout(ctx, 100*time.Millisecond)
		msgType, message, err := conn.Read(readCtx)
		readCancel()

		if err != nil {
			if ctx.Err() != nil {
				// Main context expired, we're done
				return intervals
			}
			if websocket.CloseStatus(err) != -1 {
				// WebSocket was closed, return what we have
				log.Printf("WebSocket closed: %v", err)
				return intervals
			}
			if !errors.Is(err, context.DeadlineExceeded) {
				log.Fatalf("WebSocket read error: %v", err)
			}
			continue
		}

		if msgType != websocket.MessageBinary {
			continue
		}

		if interval := parseMouseInterval(message); interval > 0 {
			intervals = append(intervals, interval)
		}
	}
}

func parseMouseInterval(message []byte) uint64 {
	const mouseEventSize = 17 // type(1) + delta_ns(8) + dx(4) + dy(4)
	if len(message) < mouseEventSize {
		return 0
	}

	if message[0] != eventTypeMouse {
		return 0
	}

	return binary.BigEndian.Uint64(message[1:9])
}

func generateVisualization(prefix string, intervals []uint64) error {
	gnuplotFile := prefix + ".gnuplot"
	if err := generateGnuplotScript(gnuplotFile, prefix, intervals); err != nil {
		return fmt.Errorf("generate gnuplot script: %w", err)
	}

	svgFile := prefix + ".svg"
	if err := exec.Command("gnuplot", gnuplotFile).Run(); err != nil {
		return fmt.Errorf("run gnuplot: %w", err)
	}

	return openFile(svgFile)
}

func generateGnuplotScript(filename, prefix string, intervals []uint64) error {
	if len(intervals) == 0 {
		return fmt.Errorf("no intervals to plot")
	}

	// Convert intervals to milliseconds
	msIntervals := make([]float64, len(intervals))
	for i, ns := range intervals {
		msIntervals[i] = float64(ns) / 1_000_000.0
	}

	script := formatGnuplotScript(prefix, msIntervals)
	return os.WriteFile(filename, []byte(script), 0644)
}

func formatGnuplotScript(prefix string, intervals []float64) string {
	var b strings.Builder

	// Find min/max for x-axis range
	minMs := intervals[0]
	maxMs := intervals[0]
	for _, ms := range intervals[1:] {
		if ms < minMs {
			minMs = ms
		}
		if ms > maxMs {
			maxMs = ms
		}
	}

	fmt.Fprintf(&b, `set terminal svg size 800,600 font "monospace,12"
set output "%s.svg"

set title "Mouse Movement Intervals"
set xlabel "Interval (milliseconds)"
set ylabel "Sample Index"

set logscale x 2

set grid
set key off

set xrange [%f:%f]
set format x "%%g"

# Data
$data << EOD
`, prefix, minMs*0.9, maxMs*1.1)

	// Output each interval with its index
	for i, interval := range intervals {
		fmt.Fprintf(&b, "%f %d\n", interval, i+1)
	}

	b.WriteString(`EOD

# Plot as points
plot $data using 1:2 with points pt 7 ps 0.1 lc rgb "blue"
`)

	return b.String()
}

func dumpLongestDelays(intervals []uint64, count int) {
	if len(intervals) == 0 {
		return
	}

	// Create a copy to avoid modifying the original
	sorted := make([]uint64, len(intervals))
	copy(sorted, intervals)

	// Sort in descending order
	sort.Slice(sorted, func(i, j int) bool {
		return sorted[i] > sorted[j]
	})

	// Determine how many to show
	if count > len(sorted) {
		count = len(sorted)
	}

	log.Printf("Top %d Longest Delays", count)
	for i := 0; i < count; i++ {
		ms := float64(sorted[i]) / 1_000_000.0
		log.Printf("%2d. %.3f ms (%d ns)\n", i+1, ms, sorted[i])
	}
}

func openFile(filename string) error {
	var cmd *exec.Cmd

	switch runtime.GOOS {
	case "windows":
		cmd = exec.Command("cmd", "/c", "start", filename)
	case "darwin":
		cmd = exec.Command("open", filename)
	default:
		cmd = exec.Command("xdg-open", filename)
	}

	if err := cmd.Start(); err != nil {
		return fmt.Errorf("open %s: %w", filename, err)
	}

	log.Printf("Opened %s", filename)
	return nil
}
