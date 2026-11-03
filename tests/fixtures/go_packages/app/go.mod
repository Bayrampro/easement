module example.com/app

go 1.22

require (
	example.com/lib v1.0.0
	github.com/lib/pq v1.10.0
	example.com/internal v1.0.0
)

replace example.com/internal => ../internal
