module Wid
  module Nodes
  end
end

require_relative "nodes/node"
require_relative "nodes/program"
require_relative "nodes/expression"

Dir[File.join(File.dirname(__FILE__), "nodes/*.rb")].each do |file|
  require_relative file
end
