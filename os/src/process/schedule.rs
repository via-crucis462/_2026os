

pub struct TaskPool {
    inner: MPSafeCell<Vec<Task>>,
}

pub struct Scheduler {
    task_pool: TaskPool,
}