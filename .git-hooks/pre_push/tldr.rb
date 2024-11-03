module Overcommit::Hook::PrePush
  class Tldr < Base
    def run
      result = if @config["include"]
        execute(command, args: applicable_files)
      else
        execute(command)
      end

      return :pass if result.success?

      output = result.stdout + result.stderr
      [:fail, output]
    end
  end
end
