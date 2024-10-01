module Wid
  module Nodes
  end
end

require_relative 'nodes/node'
require_relative 'nodes/shared/expression_collection'
require_relative 'nodes/program'
require_relative 'nodes/expression'

require_relative 'nodes/binary_operator'
require_relative 'nodes/nil'
require_relative 'nodes/number'
require_relative 'nodes/string'
require_relative 'nodes/identifier'
require_relative 'nodes/function_call'
require_relative 'nodes/var_binding'
