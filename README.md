# rust-bgzip

Basic reimplementation of
[htslib](https://github.com/samtools/htslib.git)'s `bgzip` in rust.

Written as a learning exercise: you should definitely just use the
official `bgzip` rather than this, and any commentary in this project
is mostly academic masturbation.


# Algorithm

Inefficient multi-pass alg. 

- A "master" thread reads through the entire file, finding the offsets
  BGZF blocks and stores them in a `Vec`.
  
- The `Vec` is then immutably shared between threads and an `Atomic`
  counter is used to coordinate which block the thread should
  decompress.
  
- Workers decompress the blocks and place their own (per-worker)
  output queue

- The "master" thread polls each worker's output queue for the next
  block in turn. This ensures the blocks are dequeued in-order.
  
The only benefit of this approach is that it involves no mutexes. A
better implementation would use a work-stealing queue.


# Performance

Similar performance to `bgzip`. 

Benchmarks ran on a stanard XPS13 laptop, 180 MB input file. `bgzip`
built from htslib `master` `a840aa81` (standard `make bgzip` command,
O2 optimization):

`sudo perf stat -r 100 bgzip -cd -@8 input.gz > /dev/null`:

```
       4335.994891      task-clock (msec)         #    7.359 CPUs utilized            ( +-  0.27% )
            26,580      context-switches          #    0.006 M/sec                    ( +-  1.38% )
               204      cpu-migrations            #    0.047 K/sec                    ( +-  5.29% )
             1,359      page-faults               #    0.313 K/sec                    ( +-  0.07% )
     9,897,079,178      cycles                    #    2.283 GHz                      ( +-  0.11% )
     9,445,962,551      instructions              #    0.95  insn per cycle           ( +-  0.01% )
     1,283,369,450      branches                  #  295.980 M/sec                    ( +-  0.02% )
       134,435,227      branch-misses             #   10.48% of all branches          ( +-  0.02% )

       0.589234922 seconds time elapsed                                          ( +-  0.53% )
```

`sudo perf stat -r 100 rbgzip -cd  input.gz > /dev/null`:

```
       4522.045723      task-clock (msec)         #    7.833 CPUs utilized            ( +-  0.14% )
             5,022      context-switches          #    0.001 M/sec                    ( +-  0.46% )
                 3      cpu-migrations            #    0.001 K/sec                    ( +- 14.58% )
             4,366      page-faults               #    0.965 K/sec                    ( +-  1.42% )
    10,362,011,946      cycles                    #    2.291 GHz                      ( +-  0.02% )
    11,903,164,619      instructions              #    1.15  insn per cycle           ( +-  0.00% )
     1,643,932,554      branches                  #  363.537 M/sec                    ( +-  0.00% )
       128,840,127      branch-misses             #    7.84% of all branches          ( +-  0.02% )

       0.577290012 seconds time elapsed
```
