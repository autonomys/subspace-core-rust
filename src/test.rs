use async_std::task;
use futures::join;
use std::thread;

async fn run_one() {}

async fn run_two() {}

async fn run_tasks_in_thread() {
    thread::spawn(move || {
        task::spawn(async {
            let async_process_one = run_one();
            let async_process_two = run_two();

            join!(async_process_one, async_process_two);
        });
    });
}
