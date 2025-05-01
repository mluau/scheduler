use mlua::prelude::*;
use std::cell::RefCell;

#[derive(Clone, Debug)]
pub enum ScheduleOptions {
    // None
    None,
    // task.wait semantics
    Wait { args: mlua::MultiValue },
    // task.delay semantics
    Delay { args: mlua::MultiValue },    
}

#[derive(Clone, Debug)]
pub enum TaskResult {
    None,
    Ok(LuaMultiValue),
    Err(LuaValue)
}

/// To be used in the future
pub struct Task {
    thread: LuaThread,
    result: RefCell<TaskResult>,
}

impl Task {
    pub fn new(thread: LuaThread) -> Self {
        Self { thread, result: RefCell::new(TaskResult::None) }
    }

    pub fn thread(&self) -> &LuaThread {
        &self.thread
    }

    pub fn get_result(&self) -> TaskResult {
        self.result.borrow().clone()
    }
}

impl IntoLua for Task {
    fn into_lua(self, lua: &Lua) -> LuaResult<LuaValue> {
        let inner = self.result.into_inner();

        let task_table = lua.create_table()?;
        task_table.set("co", self.thread)?;
        
        match inner {
            TaskResult::Ok(value) => task_table.set("success", {
                let tab = lua.create_sequence_from(value.iter())?;
                tab.set("n", value.len())?;
                LuaValue::Table(tab)
            })?,
            TaskResult::Err(err) => task_table.set("error", err)?,
            _ => {}
        }

        Ok(LuaValue::Table(task_table))
    }
}